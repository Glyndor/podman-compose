//! `config` rendering: validate-only, list projections (`--services`,
//! `--volumes`, `--images`, `--profiles`, `--hash`), and the resolved compose
//! file in YAML/JSON with unset keys pruned and inline secrets redacted. Split
//! out of `startup` so each file stays within the source line limit.

use std::path::Path;

use sha2::{Digest, Sha256};

use super::config_normalize::{quote_yaml11_booleans, resolve_bind_sources};
use crate::cli::ConfigFormat;

/// Output selectors for `config`, mirroring the mutually-exclusive `docker
/// compose config` list modes. The first set selector wins, in the order
/// services, volumes, images, profiles, hash.
#[derive(Default)]
pub(crate) struct ConfigOutput {
	/// `--services`: print the service names.
	pub services: bool,
	/// `--volumes`: print the named-volume keys.
	pub volumes: bool,
	/// `--images`: print each service's image reference.
	pub images: bool,
	/// `--profiles`: print the declared profile names.
	pub profiles: bool,
	/// `--hash`: print the config hash of all services ("*") or a comma-separated
	/// subset.
	pub hash: Option<String>,
	/// `--quiet`: validate only, print nothing.
	pub quiet: bool,
}

/// Render `config`: validate-only (`--quiet`), a list projection (`--services`,
/// `--volumes`, `--images`, `--profiles`, `--hash`), or the resolved compose file
/// in YAML/JSON with inline secret content redacted.
pub(crate) fn render_config(
	file: &podup::compose::types::ComposeFile,
	format: &ConfigFormat,
	out: &ConfigOutput,
	project: &str,
	base_dir: &Path,
) -> podup::Result<()> {
	// Reaching here means the file parsed and merged cleanly. Run the full
	// config-time validation (non-empty services, image-or-build, service-name
	// charset, port ranges, undefined volume/network references, and an acyclic
	// dependency graph) before the `--quiet`/projection short-circuits, so
	// validate-only (`--quiet`) actually validates — matching `docker compose config`.
	podup::validate_config(file)?;
	// Surface the active host-binding / privilege-escalation modes for every
	// service at the default log level (`warn`), so CI logs picking up
	// `podup config` output see them even when the operator never ran an
	// `up`. `config` is the "show me what will happen" command, so this
	// surface is unaffected by `--no-warn` (which exists to silence the
	// per-run copy on `up`/`create`/`run`/`exec`, not this one).
	surface_host_modes(file);
	if out.quiet {
		return Ok(());
	}
	if out.services {
		for name in file.services.keys() {
			println!("{name}");
		}
		return Ok(());
	}
	if out.volumes {
		for name in file.volumes.keys() {
			println!("{name}");
		}
		return Ok(());
	}
	if out.images {
		for (name, svc) in &file.services {
			let image = svc
				.image
				.clone()
				.unwrap_or_else(|| format!("{name}:latest"));
			println!("{image}");
		}
		return Ok(());
	}
	if out.profiles {
		let mut profiles: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
		for svc in file.services.values() {
			for p in &svc.profiles {
				profiles.insert(p.as_str());
			}
		}
		for p in profiles {
			println!("{p}");
		}
		return Ok(());
	}
	if let Some(selector) = &out.hash {
		return render_config_hash(file, selector);
	}
	let mut redacted = file.clone();
	// Surface the resolved project name in the rendered output, like
	// `docker compose config`, rather than the file's literal `name:` (or none).
	redacted.name = Some(project.to_string());
	// Don't echo keys the diagnostics pass warned were ignored: the rendered
	// config should reflect what podup actually applies, and re-feeding it must
	// not re-trigger the same warning. `x-*` extensions are kept.
	redacted.strip_ignored_unknown_keys();
	// Resolve relative bind-mount sources to absolute paths against the project
	// directory, like `docker compose config`. Runtime mounting is unaffected —
	// this only normalizes the rendered output.
	resolve_bind_sources(&mut redacted, base_dir);
	redacted.redact_inline_content();
	let rendered = match format {
		ConfigFormat::Json => {
			let mut v = serde_json::to_value(&redacted).map_err(|e| {
				podup::ComposeError::Unsupported(format!("failed to render config as JSON: {e}"))
			})?;
			prune_json_nulls(&mut v);
			serde_json::to_string_pretty(&v).map_err(|e| {
				podup::ComposeError::Unsupported(format!("failed to render config as JSON: {e}"))
			})?
		}
		ConfigFormat::Yaml => {
			let mut v: serde_yaml::Value =
				serde_yaml::to_value(&redacted).map_err(podup::ComposeError::Parse)?;
			prune_yaml_nulls(&mut v);
			let yaml = serde_yaml::to_string(&v).map_err(podup::ComposeError::Parse)?;
			// serde_yaml_ng emits YAML 1.2, where `yes`/`no`/`on`/`off` are plain
			// strings and stay unquoted. A strict YAML 1.1 reader (docker compose's
			// emitter among them) would misread those as booleans, so quote any
			// string scalar that looks like a YAML 1.1 boolean to match.
			quote_yaml11_booleans(&yaml)
		}
	};
	println!("{rendered}");
	Ok(())
}

/// SHA-256 of a service's resolved configuration, hex-encoded. Used by
/// `config --hash` so a deploy pipeline can detect a changed service. Pure so it
/// is unit-tested.
fn service_config_hash(svc: &podup::compose::types::Service) -> podup::Result<String> {
	// Not `unwrap_or_default()`: an empty vec hashes to the SHA-256 of the empty
	// string, which looks like a perfectly stable hash. A deploy pipeline keyed
	// on `config --hash` would quietly stop noticing that the service changed.
	let json = serde_json::to_vec(svc)
		.map_err(|e| podup::ComposeError::Build(format!("could not hash service config: {e}")))?;
	let hash = Sha256::digest(&json)
		.iter()
		.map(|b| format!("{b:02x}"))
		.collect();
	Ok(hash)
}

/// `config --hash`: print `SERVICE HASH` for all services ("*") or the given
/// comma-separated subset (an unknown service name is an error).
fn render_config_hash(
	file: &podup::compose::types::ComposeFile,
	selector: &str,
) -> podup::Result<()> {
	let names: Vec<String> = if selector == "*" {
		file.services.keys().cloned().collect()
	} else {
		selector
			.split(',')
			.map(|s| s.trim().to_string())
			.filter(|s| !s.is_empty())
			.collect()
	};
	for name in names {
		let svc = file
			.services
			.get(&name)
			.ok_or_else(|| podup::ComposeError::ServiceNotFound(name.clone()))?;
		println!("{name} {}", service_config_hash(svc)?);
	}
	Ok(())
}

/// Drop unset keys from a JSON value so `config` output omits them (like
/// `docker compose config`) instead of a wall of `field: null` and empty
/// `field: {}` sections. Recurses first so a section that becomes empty once its
/// own nulls are dropped is itself dropped.
fn prune_json_nulls(v: &mut serde_json::Value) {
	prune_json(v, false);
}

/// `preserve_nulls` keeps null leaves at the current mapping level. It is set for
/// the value under an `environment:` key so a map-form host-passthrough var
/// (`MYVAR:` → null) is not stripped from the output — it is forwarded at runtime,
/// so `config` must show it, matching docker compose (which never drops the key).
///
/// It is set for `networks:` for the same reason: a null value there means
/// "attach with default options", not "nothing". Dropping it removed a network
/// the service is genuinely on — visible once merging could produce a map mixing
/// a configured network with a bare one (#1078).
fn prune_json(v: &mut serde_json::Value, preserve_nulls: bool) {
	match v {
		serde_json::Value::Object(map) => {
			for (k, val) in map.iter_mut() {
				prune_json(val, k == "environment" || k == "networks");
			}
			if !preserve_nulls {
				map.retain(|_, val| !is_empty_json(val));
			}
		}
		serde_json::Value::Array(arr) => {
			for val in arr.iter_mut() {
				prune_json(val, false);
			}
		}
		_ => {}
	}
}

fn is_empty_json(v: &serde_json::Value) -> bool {
	match v {
		serde_json::Value::Null => true,
		serde_json::Value::Object(m) => m.is_empty(),
		// An empty array is kept: an explicit `command: []`/`entrypoint: []`
		// overrides the image's value, so dropping it would change meaning.
		_ => false,
	}
}

/// The YAML counterpart of [`prune_json_nulls`].
fn prune_yaml_nulls(v: &mut serde_yaml::Value) {
	prune_yaml(v, false);
}

/// YAML counterpart of [`prune_json`]; `preserve_nulls` exempts an
/// `environment:` map's null (host-passthrough) values from being dropped.
fn prune_yaml(v: &mut serde_yaml::Value, preserve_nulls: bool) {
	match v {
		serde_yaml::Value::Mapping(map) => {
			for (k, val) in map.iter_mut() {
				let child_preserve = matches!(k.as_str(), Some("environment" | "networks"));
				prune_yaml(val, child_preserve);
			}
			if !preserve_nulls {
				let drop: Vec<serde_yaml::Value> = map
					.iter()
					.filter(|(_, val)| is_empty_yaml(val))
					.map(|(k, _)| k.clone())
					.collect();
				for k in drop {
					map.remove(&k);
				}
			}
		}
		serde_yaml::Value::Sequence(seq) => {
			for val in seq.iter_mut() {
				prune_yaml(val, false);
			}
		}
		_ => {}
	}
}

fn is_empty_yaml(v: &serde_yaml::Value) -> bool {
	match v {
		serde_yaml::Value::Null => true,
		serde_yaml::Value::Mapping(m) => m.is_empty(),
		// Keep empty sequences: an explicit `command: []`/`entrypoint: []`
		// overrides the image's value, so dropping it would change meaning.
		_ => false,
	}
}

/// Walk every service and emit one `tracing::warn!` per active host-binding
/// mode. The warning is the same line the live `up` engine emits, so an
/// operator reading `config` output before running `up` sees the same set of
/// flags. `config` does not honour `--no-warn` for this surface — the whole
/// point of the command is to surface what is about to run.
fn surface_host_modes(file: &podup::compose::types::ComposeFile) {
	podup::surface_host_modes(file);
}

#[cfg(test)]
#[path = "config_render_tests.rs"]
mod tests;
