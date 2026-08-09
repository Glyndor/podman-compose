//! Name, path, link, and config-hash resolution for container creation.

use std::collections::HashMap;
use std::path::Path;

use crate::compose::types::{ComposeFile, Service};
use crate::env_file;
use crate::error::{ComposeError, Result};

/// Resolve a named-volume reference to the volume name `create_volumes`
/// produced: a custom `name:`, the raw name for an external volume, or the
/// `{project}_{name}` form.
///
/// An empty reference is an anonymous volume (an image `VOLUME` directive or a
/// colon-less short-form mount); it carries no name, so Podman assigns one and
/// it is returned unchanged. A non-empty reference that is *not* declared under
/// the top-level `volumes:` map is rejected, matching the compose-spec: leaving
/// it unprefixed would make Podman auto-create a bare, unlabelled global volume
/// that escapes project namespacing and is never reclaimed by `down -v`.
pub(super) fn resolve_volume_name(
	reference: &str,
	project: &str,
	file: &ComposeFile,
) -> Result<String> {
	if reference.is_empty() {
		return Ok(String::new());
	}
	match file.volumes.get(reference) {
		Some(cfg) => Ok(
			if let Some(name) = cfg.as_ref().and_then(|c| c.name.as_deref()) {
				name.to_string()
			} else if cfg.as_ref().and_then(|c| c.external).unwrap_or(false) {
				reference.to_string()
			} else {
				format!("{project}_{reference}")
			},
		),
		None => Err(ComposeError::Unsupported(format!(
			"service refers to undefined volume \"{reference}\"; declare it under the \
			 top-level `volumes:` key, or use a bind mount or anonymous volume instead"
		))),
	}
}

/// Resolve a bind-mount source path: expand a leading `~`, then make a relative
/// path absolute against the project base directory. Absolute paths (including
/// staged secret/config files) are returned unchanged.
pub(crate) fn resolve_bind_source(src: &str, base_dir: &Path) -> String {
	if src.is_empty() {
		return src.to_string();
	}
	let expanded = if let Some(rest) = src.strip_prefix("~/") {
		// Join with the platform separator rather than hardcoding `/`, and look
		// up the home directory in a platform-correct way (USERPROFILE on
		// native Windows, where HOME is usually unset).
		match home_dir() {
			Some(home) => home.join(rest).to_string_lossy().into_owned(),
			None => src.to_string(),
		}
	} else if src == "~" {
		home_dir()
			.map(|h| h.to_string_lossy().into_owned())
			.unwrap_or_else(|| src.to_string())
	} else {
		src.to_string()
	};
	if Path::new(&expanded).is_absolute() {
		expanded
	} else {
		base_dir.join(&expanded).to_string_lossy().into_owned()
	}
}

/// The current user's home directory. Prefers `HOME` (set on Unix and most
/// shells), falling back to `USERPROFILE` for native Windows where `HOME` is
/// usually absent. Empty values are treated as unset.
fn home_dir() -> Option<std::path::PathBuf> {
	std::env::var_os("HOME")
		.or_else(|| std::env::var_os("USERPROFILE"))
		.filter(|v| !v.is_empty())
		.map(std::path::PathBuf::from)
}

/// Resolve a service's `links` to concrete container references.
///
/// A compose `links:` entry names a sibling service; it is rewritten to that
/// service's container name with the service name kept as the network alias
/// (`{container}:{alias}`), so the linked container is reachable by the compose
/// service name. `external_links` reference containers outside the project and
/// are passed through verbatim.
pub(super) fn resolve_links(service: &Service, file: &ComposeFile, project: &str) -> Vec<String> {
	let mut links: Vec<String> = service
		.links
		.iter()
		.map(|link| {
			let (target, alias) = link.split_once(':').unwrap_or((link, link));
			let container = file
				.services
				.get(target)
				.map(|svc| {
					svc.container_name
						.clone()
						// Auto-generated container names are always index-suffixed;
						// a link/volumes_from references the first replica.
						.unwrap_or_else(|| format!("{project}-{target}-1"))
				})
				.unwrap_or_else(|| target.to_string());
			format!("{container}:{alias}")
		})
		.collect();
	links.extend(service.external_links.iter().cloned());
	links
}

/// Resolve a service's `volumes_from` entries to concrete container references.
///
/// libpod's `SpecGenerator.volumes_from` expects container names, but compose
/// `volumes_from:` names sibling services. Each entry may be `<service>`,
/// `service:<name>`, or `container:<name>`, any of which may carry a trailing
/// `:ro`/`:rw` access-mode suffix. The bare-service and `service:` forms are
/// rewritten to the referenced service's container name (an explicit
/// `container_name:` is honoured, otherwise the first replica
/// `{project}-{service}-1`), preserving
/// the access mode. The `container:` form already names a container outside the
/// project and is passed through verbatim, as is any service name that is not
/// declared in this compose file.
pub(super) fn resolve_volumes_from(
	service: &Service,
	file: &ComposeFile,
	project: &str,
) -> Vec<String> {
	service
		.volumes_from
		.iter()
		.map(|entry| {
			// Split off a trailing access mode so it survives the rewrite.
			let (reference, mode) = match entry.rsplit_once(':') {
				Some((head, tail @ ("ro" | "rw"))) => (head, Some(tail)),
				_ => (entry.as_str(), None),
			};
			let resolved = if let Some(name) = reference.strip_prefix("container:") {
				// Already a concrete container outside the project: pass through.
				name.to_string()
			} else {
				let target = reference.strip_prefix("service:").unwrap_or(reference);
				file.services
					.get(target)
					.map(|svc| {
						svc.container_name
							.clone()
							// Auto-generated container names are always index-suffixed;
							// a link/volumes_from references the first replica.
							.unwrap_or_else(|| format!("{project}-{target}-1"))
					})
					// Unknown service: leave the reference untouched.
					.unwrap_or_else(|| target.to_string())
			};
			match mode {
				Some(mode) => format!("{resolved}:{mode}"),
				None => resolved,
			}
		})
		.collect()
}

/// Stable content hash of a service definition, stored as the
/// `podup.config-hash` label. On `up`, comparing this against the label on an
/// existing container tells podup whether the service configuration changed
/// and the container must be recreated, or is unchanged and can be left as is.
///
/// The resolved bytes of any inline `content:`/`environment:` secret or config
/// the service references are folded in, so rotating an inline value recreates
/// the container to pick it up. Previously these were live host bind-mounts, so
/// a re-`up` reflected the change without recreation; now they are point-in-time
/// Podman-native secrets, so the recreate must be driven by the hash. `file:`
/// sources stay live bind-mounts and `external:` sources are by-reference, so
/// neither needs to influence the hash.
pub(crate) fn config_hash(service: &Service, file: &ComposeFile) -> Result<String> {
	use sha2::{Digest, Sha256};
	let mut hasher = Sha256::new();
	// Canonicalise through `serde_json::Value` first: `Value::Object` is
	// backed by a `BTreeMap` (see `serde_json::Map`), so a `to_value` then
	// `to_vec` round-trip emits map keys in lexicographic order regardless of
	// how the service's `HashMap`-typed fields happen to iterate. A direct
	// `to_vec(&service)` walks the field's `HashMap` directly and produces
	// different bytes for different iteration orders, which would flap the
	// hash on every parse and trigger spurious recreates. The double
	// serialisation is therefore load-bearing for the stable-hash invariant
	// (#1364 deferred the proposed optimisation — see
	// `config_hash_stable_despite_map_field_order`). Fail closed if either
	// step fails (e.g. a non-scalar mapping key in an `x-` extension):
	// returning an empty/default hash would make distinct services hash
	// identically and silently suppress recreation and inline-secret rotation.
	let serialized = serde_json::to_value(service)
		.and_then(|v| serde_json::to_vec(&v))
		.map_err(|e| ComposeError::Unsupported(format!("cannot hash service config: {e}")))?;
	hasher.update(&serialized);
	for secret_ref in &service.secrets {
		if let Some(def) = file.secrets.get(secret_ref.source()) {
			hash_inline_payload(
				&mut hasher,
				b"secret",
				secret_ref.source(),
				def.content.as_deref(),
				def.environment.as_deref(),
			);
		}
	}
	for config_ref in &service.configs {
		if let Some(def) = file.configs.get(config_ref.source()) {
			hash_inline_payload(
				&mut hasher,
				b"config",
				config_ref.source(),
				def.content.as_deref(),
				def.environment.as_deref(),
			);
		}
	}
	Ok(hasher
		.finalize()
		.iter()
		.map(|b| format!("{b:02x}"))
		.collect())
}

/// Fold an inline secret/config's resolved bytes into the config hasher. Inline
/// `content:` contributes its literal bytes; `environment:` contributes the
/// current value of the named variable (empty if unset — `up` errors on a
/// genuinely missing var later). `file:`/`external:` sources contribute nothing.
fn hash_inline_payload(
	hasher: &mut sha2::Sha256,
	kind: &[u8],
	name: &str,
	content: Option<&str>,
	environment: Option<&str>,
) {
	use sha2::Digest;
	// The environment-sourced branch holds the resolved `String` in a local so
	// the `&[u8]` view stays alive across `update()` (#1364 — also E0716
	// otherwise: a temporary `as_bytes()` is freed at end of statement).
	let env_value;
	let payload: Option<&[u8]> = match (content, environment) {
		(Some(c), _) => Some(c.as_bytes()),
		(None, Some(var)) => {
			env_value = std::env::var(var).unwrap_or_default();
			Some(env_value.as_bytes())
		}
		(None, None) => None,
	};
	if let Some(payload) = payload {
		hasher.update(kind);
		hasher.update(name.as_bytes());
		// Length-prefix so (name, payload) pairs cannot be confused across refs.
		hasher.update((payload.len() as u64).to_le_bytes());
		// `update` takes `impl AsRef<[u8]>`; `&[u8]` skips the `.to_vec()`
		// round-trip the previous code paid (#1364).
		hasher.update(payload);
	}
}

pub(super) fn build_env(service: &Service, base_dir: &Path) -> Result<Vec<String>> {
	let entries = service.env_file.to_entries();
	let env_file_vars = if !entries.is_empty() {
		env_file::load_env_file_entries(&entries, base_dir)?
	} else {
		HashMap::new()
	};
	Ok(env_file::merge_env(
		service.environment.to_map(),
		env_file_vars,
	))
}

/// Resolve a compose `stop_signal:` value to its numeric `syscall.Signal`.
///
/// libpod's `SpecGenerator.stop_signal` is an integer; sending the signal name
/// as a string returns HTTP 500. Accepts a bare number (`"15"`), a signal name
/// with or without the `SIG` prefix (`"SIGTERM"`, `"term"`), case-insensitively.
/// An unrecognized name returns a clear [`ComposeError::Unsupported`] rather than
/// silently dropping the value.
pub(super) fn resolve_stop_signal(signal: &str) -> Result<i64> {
	let trimmed = signal.trim();
	if let Ok(num) = trimmed.parse::<i64>() {
		return Ok(num);
	}
	let upper = trimmed.to_ascii_uppercase();
	let name = upper.strip_prefix("SIG").unwrap_or(&upper);
	signal_number(name)
		.ok_or_else(|| ComposeError::Unsupported(format!("unknown stop_signal '{signal}'")))
}

/// First realtime signal number on Linux/glibc (`SIGRTMIN`).
const SIGRTMIN: i64 = 34;
/// Last realtime signal number on Linux/glibc (`SIGRTMAX`).
const SIGRTMAX: i64 = 64;

/// Map a bare (no `SIG` prefix) upper-case signal name to its Linux number.
///
/// In addition to the named POSIX signals, the realtime signals are resolved:
/// `RTMIN`/`RTMAX` and the offset forms `RTMIN+N`/`RTMAX-N`, matching how
/// docker-compose and Podman interpret them. The computed number must stay within
/// the realtime range [`SIGRTMIN`]..=[`SIGRTMAX`]; an out-of-range offset (e.g.
/// `RTMIN+40`) resolves to `None` so the caller reports it as unknown.
fn signal_number(name: &str) -> Option<i64> {
	if let Some(n) = realtime_signal_number(name) {
		return Some(n);
	}
	let n = match name {
		"HUP" => 1,
		"INT" => 2,
		"QUIT" => 3,
		"ILL" => 4,
		"TRAP" => 5,
		"ABRT" | "IOT" => 6,
		"BUS" => 7,
		"FPE" => 8,
		"KILL" => 9,
		"USR1" => 10,
		"SEGV" => 11,
		"USR2" => 12,
		"PIPE" => 13,
		"ALRM" => 14,
		"TERM" => 15,
		"STKFLT" => 16,
		"CHLD" | "CLD" => 17,
		"CONT" => 18,
		"STOP" => 19,
		"TSTP" => 20,
		"TTIN" => 21,
		"TTOU" => 22,
		"URG" => 23,
		"XCPU" => 24,
		"XFSZ" => 25,
		"VTALRM" => 26,
		"PROF" => 27,
		"WINCH" => 28,
		"IO" | "POLL" => 29,
		"PWR" => 30,
		"SYS" => 31,
		_ => return None,
	};
	Some(n)
}

/// Resolve a realtime signal name (`RTMIN`, `RTMAX`, `RTMIN+N`, `RTMAX-N`) to its
/// number, or `None` if `name` is not a realtime form or the computed number
/// falls outside the realtime range [`SIGRTMIN`]..=[`SIGRTMAX`].
fn realtime_signal_number(name: &str) -> Option<i64> {
	let (base, rest) = if let Some(rest) = name.strip_prefix("RTMIN") {
		(SIGRTMIN, rest)
	} else {
		let rest = name.strip_prefix("RTMAX")?;
		(SIGRTMAX, rest)
	};
	let number = if rest.is_empty() {
		base
	} else {
		// An offset must be `+N` (only valid after RTMIN) or `-N` (only valid
		// after RTMAX), matching POSIX/glibc's `SIGRTMIN+n` / `SIGRTMAX-n`.
		let (sign, digits) = rest.split_at(1);
		let offset: i64 = digits.parse().ok()?;
		match (base == SIGRTMIN, sign) {
			(true, "+") => base + offset,
			(false, "-") => base - offset,
			_ => return None,
		}
	};
	(SIGRTMIN..=SIGRTMAX).contains(&number).then_some(number)
}

#[cfg(test)]
mod tests {
	use super::{
		config_hash, resolve_links, resolve_stop_signal, resolve_volume_name, resolve_volumes_from,
	};
	use crate::parse_str;

	#[test]
	fn stop_signal_resolves_names_numbers_and_prefixes() {
		// Named, case-insensitive, with and without the SIG prefix.
		assert_eq!(resolve_stop_signal("SIGTERM").unwrap(), 15);
		assert_eq!(resolve_stop_signal("TERM").unwrap(), 15);
		assert_eq!(resolve_stop_signal("sigterm").unwrap(), 15);
		assert_eq!(resolve_stop_signal("SIGKILL").unwrap(), 9);
		assert_eq!(resolve_stop_signal("hup").unwrap(), 1);
		assert_eq!(resolve_stop_signal("SIGUSR1").unwrap(), 10);
		assert_eq!(resolve_stop_signal("SIGUSR2").unwrap(), 12);
		// Bare numbers pass through (optionally surrounded by whitespace).
		assert_eq!(resolve_stop_signal("15").unwrap(), 15);
		assert_eq!(resolve_stop_signal(" 9 ").unwrap(), 9);
	}

	#[test]
	fn stop_signal_resolves_realtime_signals() {
		// Realtime signals and their offset forms, as docker-compose/Podman accept
		// them (images on s6-overlay/tini commonly use SIGRTMIN+3).
		assert_eq!(resolve_stop_signal("SIGRTMIN").unwrap(), 34);
		assert_eq!(resolve_stop_signal("SIGRTMIN+3").unwrap(), 37);
		assert_eq!(resolve_stop_signal("SIGRTMAX").unwrap(), 64);
		assert_eq!(resolve_stop_signal("SIGRTMAX-1").unwrap(), 63);
		// The SIG prefix is optional here too.
		assert_eq!(resolve_stop_signal("RTMIN+3").unwrap(), 37);
	}

	#[test]
	fn stop_signal_rejects_out_of_range_realtime_offset() {
		// 34 + 40 = 74 is past SIGRTMAX (64), so it is rejected.
		let err = resolve_stop_signal("SIGRTMIN+40").unwrap_err();
		assert!(err.to_string().contains("SIGRTMIN+40"), "got: {err}");
		// Wrong-direction offsets are rejected too.
		assert!(resolve_stop_signal("SIGRTMIN-1").is_err());
		assert!(resolve_stop_signal("SIGRTMAX+1").is_err());
	}

	#[test]
	fn stop_signal_unknown_name_errors() {
		let err = resolve_stop_signal("SIGNOPE").unwrap_err();
		assert!(err.to_string().contains("SIGNOPE"), "got: {err}");
	}

	#[test]
	fn links_resolve_to_container_names_external_links_verbatim() {
		let file = parse_str(
			"services:\n  db:\n    image: x\n  web:\n    image: x\n    links:\n      - db\n      - db:primary\n    external_links:\n      - legacy_db:db\n",
		)
		.unwrap();
		let links = resolve_links(&file.services["web"], &file, "proj");
		// Auto-generated names are always index-suffixed; a link targets the
		// first replica.
		assert!(links.contains(&"proj-db-1:db".to_string()));
		assert!(links.contains(&"proj-db-1:primary".to_string()));
		assert!(links.contains(&"legacy_db:db".to_string()));
	}

	#[test]
	fn links_honour_custom_container_name() {
		let file = parse_str(
			"services:\n  db:\n    image: x\n    container_name: my-db\n  web:\n    image: x\n    links:\n      - db\n",
		)
		.unwrap();
		let links = resolve_links(&file.services["web"], &file, "proj");
		assert_eq!(links, vec!["my-db:db".to_string()]);
	}

	#[test]
	fn volumes_from_resolves_to_container_names() {
		let file = parse_str(
			"services:\n  db:\n    image: x\n  cache:\n    image: x\n    container_name: my-cache\n  web:\n    image: x\n    volumes_from:\n      - db\n      - cache\n",
		)
		.unwrap();
		let resolved = resolve_volumes_from(&file.services["web"], &file, "proj");
		// Bare service name resolves to the first replica `{project}-{service}-1`.
		assert!(resolved.contains(&"proj-db-1".to_string()));
		// An explicit container_name is honoured.
		assert!(resolved.contains(&"my-cache".to_string()));
	}

	#[test]
	fn volumes_from_preserves_access_mode() {
		let file = parse_str(
			"services:\n  db:\n    image: x\n  web:\n    image: x\n    volumes_from:\n      - db:ro\n      - service:db:rw\n",
		)
		.unwrap();
		let resolved = resolve_volumes_from(&file.services["web"], &file, "proj");
		// The `:ro`/`:rw` suffix survives the rewrite, and the `service:` prefix
		// is stripped before resolving.
		assert!(resolved.contains(&"proj-db-1:ro".to_string()));
		assert!(resolved.contains(&"proj-db-1:rw".to_string()));
	}

	#[test]
	fn volumes_from_passes_through_container_form_and_unknown() {
		let file = parse_str(
			"services:\n  web:\n    image: x\n    volumes_from:\n      - container:legacy-data\n      - container:legacy-data:ro\n      - missing\n",
		)
		.unwrap();
		let resolved = resolve_volumes_from(&file.services["web"], &file, "proj");
		// The `container:` form names a container outside the project verbatim.
		assert!(resolved.contains(&"legacy-data".to_string()));
		assert!(resolved.contains(&"legacy-data:ro".to_string()));
		// An unknown service is left unchanged.
		assert!(resolved.contains(&"missing".to_string()));
	}

	#[test]
	#[cfg(unix)]
	fn bind_source_resolution() {
		use super::resolve_bind_source;
		use std::path::Path;
		let base = Path::new("/srv/app");
		assert_eq!(resolve_bind_source("/abs/path", base), "/abs/path");
		assert_eq!(resolve_bind_source("./data", base), "/srv/app/./data");
		assert_eq!(resolve_bind_source("data", base), "/srv/app/data");
		// Mutating HOME via the process env races other tests under the parallel
		// runner; temp_env::with_var sets and restores it atomically.
		temp_env::with_var("HOME", Some("/home/u"), || {
			assert_eq!(resolve_bind_source("~/x", base), "/home/u/x");
			assert_eq!(resolve_bind_source("~", base), "/home/u");
		});
		// An empty source is returned verbatim (no base-dir join).
		assert_eq!(resolve_bind_source("", base), "");
	}

	#[test]
	#[cfg(unix)]
	fn bind_source_tilde_without_home_stays_literal() {
		use super::resolve_bind_source;
		use std::path::Path;
		let base = Path::new("/srv/app");
		// With no home directory resolvable, a `~`-prefixed path keeps its literal
		// form, then (being relative) is anchored to the base dir.
		temp_env::with_vars(
			[("HOME", None::<&str>), ("USERPROFILE", None::<&str>)],
			|| {
				assert_eq!(resolve_bind_source("~/x", base), "/srv/app/~/x");
				assert_eq!(resolve_bind_source("~", base), "/srv/app/~");
			},
		);
	}

	#[test]
	fn volume_name_resolution() {
		let f = parse_str(
			"services:\n  s:\n    image: x\nvolumes:\n  data:\n  ext:\n    external: true\n  custom:\n    name: my-vol\n",
		)
		.unwrap();
		assert_eq!(
			resolve_volume_name("data", "proj", &f).unwrap(),
			"proj_data"
		);
		assert_eq!(resolve_volume_name("ext", "proj", &f).unwrap(), "ext");
		assert_eq!(resolve_volume_name("custom", "proj", &f).unwrap(), "my-vol");
		// An empty reference is an anonymous volume: no name to resolve.
		assert_eq!(resolve_volume_name("", "proj", &f).unwrap(), "");
		// A non-empty reference not declared under top-level `volumes:` is
		// rejected rather than escaping into a bare, unprefixed global volume.
		let err = resolve_volume_name("missing", "proj", &f).unwrap_err();
		assert!(err.to_string().contains("missing"), "got: {err}");
	}

	#[test]
	fn config_hash_is_stable_and_sensitive() {
		let a = parse_str("services:\n  web:\n    image: nginx:1.27\n").unwrap();
		let b = parse_str("services:\n  web:\n    image: nginx:1.27\n").unwrap();
		let c = parse_str("services:\n  web:\n    image: nginx:1.28\n").unwrap();
		let ha = config_hash(&a.services["web"], &a).unwrap();
		let hb = config_hash(&b.services["web"], &b).unwrap();
		let hc = config_hash(&c.services["web"], &c).unwrap();
		assert_eq!(ha, hb, "same config produces the same hash");
		assert_ne!(ha, hc, "a changed image produces a different hash");
		assert_eq!(ha.len(), 64, "sha-256 hex is 64 chars");
	}

	#[test]
	fn config_hash_tracks_inline_secret_content() {
		// Rotating an inline `content:` secret must change the hash so the
		// container is recreated to pick up the new (point-in-time) native secret.
		let a = parse_str(
			"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    content: v1\n",
		)
		.unwrap();
		let b = parse_str(
			"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    content: v2\n",
		)
		.unwrap();
		assert_ne!(
			config_hash(&a.services["web"], &a).unwrap(),
			config_hash(&b.services["web"], &b).unwrap(),
			"changed inline secret content must change the hash",
		);
	}

	#[test]
	fn config_hash_ignores_external_secret_identity() {
		// An `external:` secret is by-reference (no payload), so it does not add
		// to the hash beyond the service's own secret list.
		let a = parse_str(
			"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n",
		)
		.unwrap();
		let b = parse_str(
			"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n    name: other\n",
		)
		.unwrap();
		// The service definition is identical; only the top-level external name
		// differs, which is resolved at attach time, not baked into the hash.
		assert_eq!(
			config_hash(&a.services["web"], &a).unwrap(),
			config_hash(&b.services["web"], &b).unwrap(),
		);
	}

	#[test]
	fn config_hash_tracks_inline_config_content() {
		// The same recreate-on-rotation contract as inline secrets applies to
		// inline `configs:` content.
		let a = parse_str(
			"services:\n  web:\n    image: x\n    configs: [cfg]\nconfigs:\n  cfg:\n    content: a\n",
		)
		.unwrap();
		let b = parse_str(
			"services:\n  web:\n    image: x\n    configs: [cfg]\nconfigs:\n  cfg:\n    content: b\n",
		)
		.unwrap();
		assert_ne!(
			config_hash(&a.services["web"], &a).unwrap(),
			config_hash(&b.services["web"], &b).unwrap(),
			"changed inline config content must change the hash",
		);
	}

	#[test]
	fn config_hash_tracks_environment_sourced_secret() {
		// An `environment:`-sourced secret folds the *current* value of the named
		// variable into the hash, so a change to that variable recreates.
		let file = parse_str(
			"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    environment: PODUP_TEST_SECRET\n",
		)
		.unwrap();
		let with_a = temp_env::with_var("PODUP_TEST_SECRET", Some("alpha"), || {
			config_hash(&file.services["web"], &file).unwrap()
		});
		let with_b = temp_env::with_var("PODUP_TEST_SECRET", Some("beta"), || {
			config_hash(&file.services["web"], &file).unwrap()
		});
		assert_ne!(
			with_a, with_b,
			"a changed environment-sourced secret value must change the hash",
		);
	}

	#[test]
	fn config_hash_stable_despite_map_field_order() {
		// `storage_opt` is a HashMap; canonical serialisation must sort its keys
		// so the hash does not flap and trigger a spurious recreate on `up`.
		let a = parse_str(
			"services:\n  web:\n    image: x\n    storage_opt:\n      size: \"10G\"\n      foo: bar\n      baz: qux\n",
		)
		.unwrap();
		let b = parse_str(
			"services:\n  web:\n    image: x\n    storage_opt:\n      baz: qux\n      size: \"10G\"\n      foo: bar\n",
		)
		.unwrap();
		assert_eq!(
			config_hash(&a.services["web"], &a).unwrap(),
			config_hash(&b.services["web"], &b).unwrap(),
			"hash must be independent of storage_opt key order",
		);
	}
}
