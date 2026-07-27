//! Pure mapping from compose `secrets:`/`configs:` references to native-secret
//! plans. No daemon access, so the mapping is unit-testable; the create and
//! preflight side effects live in [`super`]'s `Engine` impl.

use std::path::{Path, PathBuf};

use crate::compose::types::{ComposeFile, Service, ServiceConfigRef, ServiceSecretRef};
use crate::error::{ComposeError, Result};

use super::super::staging;

/// Podman's hard limit on secret payload size (from `containers/common`): the
/// payload must be larger than 0 and strictly smaller than this many bytes.
pub(super) const MAX_SECRET_BYTES: usize = 512_000;

/// Where the bytes of a podup-created secret come from.
///
/// A `file:` source carries the resolved host path rather than its contents:
/// this module maps compose to plans with no I/O at all, which is what keeps the
/// mapping unit-testable, so the read happens in the effectful layer that creates
/// the secret.
pub(super) enum Payload {
	/// Inline `content:`/`environment:` — the bytes, already resolved.
	Inline(Vec<u8>),
	/// `file:` — the resolved host path to read at creation time.
	File(PathBuf),
}

/// A planned native secret for a service: the Podman secret `source` to attach,
/// the in-container `target`, optional permissions, and — for every source podup
/// creates itself — the `payload` to create under `source`. `external: true`
/// references carry no payload (the secret must pre-exist).
pub(super) struct NativePlan {
	pub(super) source: String,
	pub(super) target: String,
	pub(super) mode: Option<u32>,
	pub(super) uid: Option<u32>,
	pub(super) gid: Option<u32>,
	pub(super) payload: Option<Payload>,
}

/// The fields of a `secrets:`/`configs:` definition that decide where its bytes
/// come from. A borrowed view, so the two distinct compose types (`SecretConfig`
/// and `ConfigConfig`) resolve through one function.
struct SourceDef<'a> {
	content: Option<&'a str>,
	environment: Option<&'a str>,
	file_source: Option<&'a str>,
	external: bool,
	external_name: Option<&'a str>,
}

/// Where a secret/config's bytes come from once the compose def is resolved.
enum Source {
	/// Inline `content:`/`environment:` — `(scoped podman name, payload bytes)`.
	Inline(String, Vec<u8>),
	/// `file:` — `(scoped podman name, resolved host path)`.
	File(String, PathBuf),
	/// `external: true` — name of the pre-existing podman secret.
	External(String),
}

/// Collect the native-secret plans for a service without touching the daemon. A
/// dangerous `mode:` (execute/setuid/setgid/sticky) is rejected here so a
/// hostile mode never reaches Podman.
pub(super) fn collect_native_plans(
	project: &str,
	service: &Service,
	file: &ComposeFile,
	base_dir: &Path,
) -> Result<Vec<NativePlan>> {
	let mut plans = Vec::new();

	for secret_ref in &service.secrets {
		let (name, target_override, mode, uid, gid) = secret_ref_parts(secret_ref);
		if let Some(def) = file.secrets.get(&name) {
			let source = resolve_source(
				project,
				"secret",
				&name,
				SourceDef {
					content: def.content.as_deref(),
					environment: def.environment.as_deref(),
					file_source: def.file.as_deref(),
					external: def.external == Some(true),
					external_name: def.name.as_deref(),
				},
				base_dir,
			)?;
			// A bare target name lands under /run/secrets/<name>, which is where
			// Podman mounts a secret referenced by name and where a `file:` source
			// landed back when it was a bind mount. Changing it would move every
			// existing project's secrets.
			push_plan(
				&mut plans,
				source,
				target_override.unwrap_or(name),
				mode,
				uid,
				gid,
			)?;
		}
	}

	for config_ref in &service.configs {
		let (name, target_override, mode, uid, gid) = config_ref_parts(config_ref);
		if let Some(def) = file.configs.get(&name) {
			let source = resolve_source(
				project,
				"config",
				&name,
				SourceDef {
					content: def.content.as_deref(),
					environment: def.environment.as_deref(),
					file_source: def.file.as_deref(),
					external: def.external == Some(true),
					external_name: def.name.as_deref(),
				},
				base_dir,
			)?;
			// Configs default to an absolute container-root path — `/name`, not
			// `/run/secrets/name`. That is what separates a config from a secret
			// here, and it is the path a `file:` config landed on when it was a
			// bind mount.
			let target = target_override.unwrap_or_else(|| format!("/{name}"));
			push_plan(&mut plans, source, target, mode, uid, gid)?;
		}
	}

	Ok(plans)
}

/// Resolve a secret/config definition to its native [`Source`]. `external`
/// wins (it may also carry a custom `name:`); every other populated source —
/// inline `content:`/`environment:` and `file:` alike — becomes a project-scoped
/// native secret. An empty def yields `None` and contributes no plan.
fn resolve_source(
	project: &str,
	kind: &str,
	name: &str,
	def: SourceDef<'_>,
	base_dir: &Path,
) -> Result<Option<Source>> {
	let SourceDef {
		content,
		environment,
		file_source,
		external,
		external_name,
	} = def;
	if external {
		return Ok(Some(Source::External(
			external_name.unwrap_or(name).to_string(),
		)));
	}
	let podup_created = content.is_some() || environment.is_some() || file_source.is_some();
	if podup_created && !staging::is_safe_project_name(name) {
		// The name becomes part of the project-scoped Podman secret name and a URL
		// query parameter, so require a bounded, well-formed identifier rather than
		// an arbitrary (possibly huge or control-laden) YAML key.
		return Err(ComposeError::Unsupported(format!(
			"{kind} name {name:?} must be ASCII alphanumeric/dash/underscore/dot, \
			 at most 128 chars, and not start with a dot"
		)));
	}
	if let Some(content) = content {
		return Ok(Some(Source::Inline(
			scoped_name(project, kind, name),
			content.as_bytes().to_vec(),
		)));
	}
	if let Some(env_var) = environment {
		let value = std::env::var(env_var).map_err(|_| {
			ComposeError::Unsupported(format!(
				"{kind} '{name}' references env var '{env_var}' which is not set"
			))
		})?;
		return Ok(Some(Source::Inline(
			scoped_name(project, kind, name),
			value.into_bytes(),
		)));
	}
	if let Some(host_path) = file_source {
		// Resolve like a bind-mount source: a relative `file:` is anchored to the
		// project dir (not the Podman service's cwd) and `~` is expanded — the same
		// handling `volumes:` gets, and the same this had when it was a bind.
		return Ok(Some(Source::File(
			scoped_name(project, kind, name),
			PathBuf::from(super::super::container::resolve_bind_source(
				host_path, base_dir,
			)),
		)));
	}
	Ok(None)
}

/// Append a [`NativePlan`] for a resolved source, dropping an empty def and
/// rejecting a dangerous `mode:` before the spec is built. `uid`/`gid` are
/// numeric in libpod, so a non-numeric value (a user/group name) is dropped to
/// the default rather than erroring.
fn push_plan(
	plans: &mut Vec<NativePlan>,
	source: Option<Source>,
	target: String,
	mode: Option<u32>,
	uid: Option<String>,
	gid: Option<String>,
) -> Result<()> {
	let (source, payload, from_file) = match source {
		None => return Ok(()),
		Some(Source::Inline(s, p)) => (s, Some(Payload::Inline(p)), false),
		Some(Source::File(s, p)) => (s, Some(Payload::File(p)), true),
		Some(Source::External(s)) => (s, None, false),
	};
	// Default to the Compose Specification's world-readable `0444` when no `mode:`
	// is given. A Podman-native secret otherwise mounts at `0000`, which a non-root
	// container user cannot read (only root reads it via DAC override), diverging
	// from docker-compose where the default is readable.
	//
	// A `file:` source is the exception: it is left unset here so the effectful
	// layer can mirror the host file's own permission bits. That keeps what the
	// container sees identical to the bind this used to be — a `0600` secret stays
	// unreadable to a non-root container user instead of being widened to `0444`.
	let mode = if from_file {
		mode
	} else {
		mode.or(Some(0o444))
	};
	if let Some(m) = mode {
		staging::reject_dangerous_secret_mode(m, &source)?;
	}
	plans.push(NativePlan {
		source,
		target,
		mode,
		uid: uid.and_then(|s| s.parse().ok()),
		gid: gid.and_then(|s| s.parse().ok()),
		payload,
	});
	Ok(())
}

/// Project-scoped Podman secret name for an inline secret/config, namespaced by
/// `kind` so a secret and a config sharing a compose name do not collide.
pub(super) fn scoped_name(project: &str, kind: &str, name: &str) -> String {
	format!("{project}_{kind}_{name}")
}

/// Reject a payload Podman would refuse (`len == 0` or `>= MAX_SECRET_BYTES`),
/// with a clearer message than the daemon's opaque 500.
pub(super) fn check_secret_size(name: &str, len: usize) -> Result<()> {
	if len == 0 || len >= MAX_SECRET_BYTES {
		return Err(ComposeError::Unsupported(format!(
			"secret '{name}' is {len} bytes; a Podman secret payload must be \
			 larger than 0 and smaller than {MAX_SECRET_BYTES} bytes"
		)));
	}
	Ok(())
}

/// Whether a secret/config def is one podup creates as a project-scoped native
/// secret — inline `content:`/`environment:` or a `file:` source. `external:`
/// wins and is never created (nor removed) by podup.
pub(super) fn is_podup_created_source(
	external: Option<bool>,
	content: Option<&str>,
	environment: Option<&str>,
	file_source: Option<&str>,
) -> bool {
	external != Some(true) && (content.is_some() || environment.is_some() || file_source.is_some())
}

/// The permission bits to mount a `file:` secret with when the compose file
/// names no `mode:` — the host file's own, so the container sees what it saw
/// when this was a bind mount.
///
/// Execute and the special bits are masked off: a secret holds data, never code,
/// and letting a `0755` host file through would trip the dangerous-mode guard and
/// fail an `up` that used to work. Unreadable metadata falls back to the Compose
/// Specification's `0444`; the create call fails right after with a clearer
/// message than a permissions guess would give.
pub(super) fn host_file_secret_mode(path: &Path) -> u32 {
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		match std::fs::metadata(path) {
			Ok(md) => md.permissions().mode() & 0o666,
			Err(_) => 0o444,
		}
	}
	#[cfg(not(unix))]
	{
		let _ = path;
		0o444
	}
}

/// Decompose a secret reference into `(name, target, mode, uid, gid)`.
fn secret_ref_parts(
	r: &ServiceSecretRef,
) -> (
	String,
	Option<String>,
	Option<u32>,
	Option<String>,
	Option<String>,
) {
	match r {
		ServiceSecretRef::Short(s) => (s.clone(), None, None, None, None),
		ServiceSecretRef::Long {
			source,
			target,
			mode,
			uid,
			gid,
		} => (
			source.clone(),
			target.clone(),
			*mode,
			uid.clone(),
			gid.clone(),
		),
	}
}

/// Decompose a config reference into `(name, target, mode, uid, gid)`.
fn config_ref_parts(
	r: &ServiceConfigRef,
) -> (
	String,
	Option<String>,
	Option<u32>,
	Option<String>,
	Option<String>,
) {
	match r {
		ServiceConfigRef::Short(s) => (s.clone(), None, None, None, None),
		ServiceConfigRef::Long {
			source,
			target,
			mode,
			uid,
			gid,
		} => (
			source.clone(),
			target.clone(),
			*mode,
			uid.clone(),
			gid.clone(),
		),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn plans(yaml: &str) -> Vec<NativePlan> {
		let file = crate::compose::parse_str_raw(yaml).unwrap();
		collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).unwrap()
	}

	/// The inline bytes of a plan's payload, or `None` for an external/file source.
	fn inline_bytes(p: &NativePlan) -> Option<&[u8]> {
		match &p.payload {
			Some(Payload::Inline(b)) => Some(b),
			_ => None,
		}
	}

	#[test]
	fn file_secret_is_a_scoped_native_secret_carrying_its_path() {
		// A `file:` secret is a project-scoped native secret like any other source.
		// The plan carries the resolved path, not the bytes: this module does no I/O,
		// so the read belongs to the layer that creates the secret.
		let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    file: ./tok.txt\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "proj_secret_tok");
		assert_eq!(p[0].target, "tok");
		assert!(
			matches!(&p[0].payload, Some(Payload::File(path)) if path == Path::new("/base/tok.txt"))
		);
	}

	#[test]
	fn file_secret_leaves_mode_unset_for_the_host_bits() {
		// With no `mode:` the plan stays unset so the effectful layer can mirror the
		// host file's own bits, rather than widening a 0600 secret to the 0444 the
		// other sources default to.
		let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    file: ./tok.txt\n");
		assert_eq!(p[0].mode, None);
	}

	#[test]
	fn file_secret_honours_an_explicit_mode() {
		let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 0400\nsecrets:\n  tok:\n    file: ./tok.txt\n");
		assert_eq!(p[0].mode, Some(0o400));
	}

	#[test]
	fn file_config_is_native_with_absolute_default_target() {
		let p = plans("services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    file: ./cfg.yaml\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "proj_config_cfg");
		assert_eq!(p[0].target, "/cfg");
	}

	#[test]
	fn file_secret_with_unsafe_name_is_rejected() {
		// The compose key becomes part of a Podman secret name for a `file:` source
		// too, so it faces the same bound as an inline one.
		let file = crate::compose::parse_str_raw(
			"services:\n  web:\n    image: x\n    secrets: ['../evil']\nsecrets:\n  '../evil':\n    file: ./tok.txt\n",
		)
		.unwrap();
		assert!(
			collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
		);
	}

	#[test]
	fn empty_def_contributes_no_plan() {
		// A def with no content:, environment:, file: or external: is not a secret
		// podup can produce anything for.
		let p =
			plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok: {}\n");
		assert!(p.is_empty());
	}

	// Unix-only: `PermissionsExt` does not exist on Windows, where a host file has
	// no mode to mirror and `host_file_secret_mode` returns the 0444 default.
	#[cfg(unix)]
	#[test]
	fn host_file_mode_masks_execute_and_special_bits() {
		// A secret holds data, never code. Mirroring a 0755 host file verbatim would
		// trip the dangerous-mode guard and fail an `up` that used to work.
		let dir = tempfile::tempdir().unwrap();
		let path = dir.path().join("s.txt");
		std::fs::write(&path, b"x").unwrap();
		for (host, want) in [
			(0o644, 0o644),
			(0o600, 0o600),
			(0o755, 0o644),
			(0o4700, 0o600),
		] {
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&path, std::fs::Permissions::from_mode(host)).unwrap();
			assert_eq!(host_file_secret_mode(&path), want, "host mode {host:o}");
		}
	}

	#[test]
	fn host_file_mode_of_a_missing_file_falls_back_to_0444() {
		assert_eq!(
			host_file_secret_mode(Path::new("/nonexistent/secret")),
			0o444
		);
	}

	#[test]
	fn inline_content_secret_is_scoped_native_with_payload() {
		// `content:` becomes a project-scoped native secret carrying the bytes;
		// the mount target defaults to the bare compose name (→ /run/secrets/tok).
		let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    content: supersecret\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "proj_secret_tok");
		assert_eq!(p[0].target, "tok");
		assert_eq!(inline_bytes(&p[0]), Some(b"supersecret".as_slice()));
	}

	#[test]
	fn inline_content_config_is_scoped_native_with_absolute_target() {
		// Configs default to an absolute container-root path.
		let p = plans("services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    content: key=value\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "proj_config_cfg");
		assert_eq!(p[0].target, "/cfg");
		assert_eq!(inline_bytes(&p[0]), Some(b"key=value".as_slice()));
	}

	#[test]
	fn env_secret_payload_comes_from_environment() {
		temp_env::with_var("PODUP_TEST_SECRET", Some("env-value"), || {
			let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    environment: PODUP_TEST_SECRET\n");
			assert_eq!(p.len(), 1);
			assert_eq!(p[0].source, "proj_secret_tok");
			assert_eq!(inline_bytes(&p[0]), Some(b"env-value".as_slice()));
		});
	}

	#[test]
	fn env_secret_missing_var_errors() {
		temp_env::with_var("PODUP_TEST_MISSING", None::<&str>, || {
			let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    environment: PODUP_TEST_MISSING\n").unwrap();
			assert!(
				collect_native_plans("proj", &file.services["web"], &file, Path::new("/base"))
					.is_err()
			);
		});
	}

	#[test]
	fn external_secret_keeps_compose_name_unscoped_no_payload() {
		// An `external: true` secret points at a pre-existing podman secret: the
		// source equals the compose name (no project scoping) and carries no
		// payload. The mount filename defaults to the compose name.
		let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "tok");
		assert_eq!(p[0].target, "tok");
		assert!(p[0].payload.is_none());
	}

	#[test]
	fn external_secret_long_form_maps_source_target_and_perms() {
		// A long-form ref overrides the mount name, a custom top-level `name:` is
		// the real podman secret, and numeric uid/gid/mode pass through. `mode:` is
		// octal notation per the Compose Specification (leading-zero `0400`).
		let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        target: app_tok\n        uid: \"100\"\n        gid: \"101\"\n        mode: 0400\nsecrets:\n  tok:\n    external: true\n    name: real_tok\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "real_tok");
		assert_eq!(p[0].target, "app_tok");
		assert_eq!(p[0].uid, Some(100));
		assert_eq!(p[0].gid, Some(101));
		assert_eq!(p[0].mode, Some(0o400));
	}

	#[test]
	fn external_config_becomes_native_with_absolute_default_target() {
		let p = plans("services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    external: true\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "cfg");
		assert_eq!(p[0].target, "/cfg");
	}

	#[test]
	fn non_numeric_uid_drops_to_default() {
		// libpod secret uid/gid are numeric; a user/group name falls back to the
		// default rather than erroring.
		let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        uid: appuser\nsecrets:\n  tok:\n    external: true\n");
		assert_eq!(p.len(), 1);
		assert!(p[0].uid.is_none());
	}

	#[test]
	fn native_secret_rejects_setuid_mode() {
		// 0o4000 (= 2048) is setuid; refused before the spec reaches Podman.
		let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 2048\nsecrets:\n  tok:\n    external: true\n").unwrap();
		assert!(
			collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
		);
	}

	#[test]
	fn native_secret_rejects_execute_mode() {
		// 0o777 (= 511) sets execute bits; a secret holds data, never code.
		let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 511\nsecrets:\n  tok:\n    external: true\n").unwrap();
		assert!(
			collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
		);
	}

	#[test]
	fn native_config_rejects_setgid_mode() {
		// External configs share the mode guard. 0o2000 (= 1024) is setgid.
		let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    configs:\n      - source: cfg\n        mode: 1024\nconfigs:\n  cfg:\n    external: true\n").unwrap();
		assert!(
			collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
		);
	}

	#[test]
	fn inline_secret_rejects_dangerous_mode() {
		// The mode guard also covers project-created inline secrets.
		let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 511\nsecrets:\n  tok:\n    content: data\n").unwrap();
		assert!(
			collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
		);
	}

	#[test]
	fn native_secret_allows_world_readable_mode() {
		// 0o444 (= 292) is the Podman/compose default for an in-container secret
		// and must be allowed (unlike the old shared-host staging path).
		let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 292\nsecrets:\n  tok:\n    external: true\n");
		assert_eq!(p[0].mode, Some(0o444));
	}

	#[test]
	fn empty_and_oversized_payloads_rejected() {
		assert!(check_secret_size("s", 0).is_err());
		assert!(check_secret_size("s", MAX_SECRET_BYTES).is_err());
		assert!(check_secret_size("s", MAX_SECRET_BYTES - 1).is_ok());
		assert!(check_secret_size("s", 1).is_ok());
	}

	#[test]
	fn inline_secret_with_unsafe_name_is_rejected() {
		// A path-traversal / control-laden key must not become a Podman secret name.
		let file = crate::compose::parse_str_raw(
			"services:\n  web:\n    image: x\n    secrets: ['../evil']\nsecrets:\n  '../evil':\n    content: data\n",
		)
		.unwrap();
		assert!(
			collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
		);
	}

	#[test]
	fn native_secret_without_mode_defaults_to_0444() {
		// The Compose Specification default is world-readable 0444; a Podman-native
		// secret otherwise mounts at 0000 and a non-root container user can't read it.
		let p = plans("services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    content: data\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].mode, Some(0o444));
	}

	#[test]
	fn secret_mode_leading_zero_is_octal() {
		// `0444` (leading-zero octal, the Compose Specification spelling) parses as
		// octal 0o444, not decimal 444 (which would fail) — issue #2.
		let p = plans("services:\n  web:\n    image: x\n    secrets:\n      - source: tok\n        mode: 0444\nsecrets:\n  tok:\n    content: data\n");
		assert_eq!(p[0].mode, Some(0o444));
	}

	#[test]
	fn is_podup_created_source_classifies_sources() {
		assert!(is_podup_created_source(None, Some("x"), None, None));
		assert!(is_podup_created_source(None, None, Some("VAR"), None));
		// A `file:` source is created by podup too, so `down` must remove it.
		assert!(is_podup_created_source(None, None, None, Some("./tok.txt")));
		assert!(!is_podup_created_source(Some(true), Some("x"), None, None));
		assert!(!is_podup_created_source(None, None, None, None));
	}
}
