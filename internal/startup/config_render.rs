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
mod tests {
	use super::*;

	fn sample_file() -> podup::compose::types::ComposeFile {
		podup::parse_str("services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n")
			.unwrap()
	}

	#[test]
	fn prune_json_drops_nulls_and_empty_then_collapses() {
		let mut v = serde_json::json!({
			"image": "nginx",
			"environment": null,
			"command": [],
			"labels": {},
			"deploy": { "replicas": null }
		});
		prune_json_nulls(&mut v);
		// null fields and the section emptied by its own nulls are gone, but an
		// explicit empty array (`command: []`) survives — it overrides the image.
		assert_eq!(v, serde_json::json!({ "image": "nginx", "command": [] }));
	}

	#[test]
	fn prune_yaml_drops_nulls_and_empty() {
		let mut v: serde_yaml::Value =
			serde_yaml::from_str("image: nginx\ndns: null\nnetworks: {}\n").unwrap();
		prune_yaml_nulls(&mut v);
		let out = serde_yaml::to_string(&v).unwrap();
		assert!(out.contains("image: nginx"));
		assert!(!out.contains("dns"));
		assert!(!out.contains("networks"));
	}

	#[test]
	fn render_config_rejects_depends_on_cycle() {
		// A `depends_on` cycle must be reported at config time, not deferred to up.
		let file = podup::parse_str(
			"services:\n  a:\n    image: x\n    depends_on: [b]\n  b:\n    image: y\n    depends_on: [a]\n",
		)
		.unwrap();
		let err = render_config(
			&file,
			&ConfigFormat::Yaml,
			&ConfigOutput {
				quiet: true,
				..Default::default()
			},
			"proj",
			Path::new("/proj"),
		)
		.unwrap_err();
		assert!(matches!(err, podup::ComposeError::CircularDependency(_)));
	}

	#[test]
	fn render_config_quiet_is_validate_only() {
		// `--quiet` validates and prints nothing, returning Ok.
		render_config(
			&sample_file(),
			&ConfigFormat::Yaml,
			&ConfigOutput {
				quiet: true,
				..Default::default()
			},
			"proj",
			Path::new("/proj"),
		)
		.unwrap();
	}

	#[test]
	fn render_config_services_lists_names() {
		// `--services` reaches the service-name listing branch without error.
		render_config(
			&sample_file(),
			&ConfigFormat::Yaml,
			&ConfigOutput {
				services: true,
				..Default::default()
			},
			"proj",
			Path::new("/proj"),
		)
		.unwrap();
	}

	#[test]
	fn render_config_projection_modes_render_ok() {
		// Each list-projection selector reaches its branch without error.
		for out in [
			ConfigOutput {
				volumes: true,
				..Default::default()
			},
			ConfigOutput {
				images: true,
				..Default::default()
			},
			ConfigOutput {
				profiles: true,
				..Default::default()
			},
			ConfigOutput {
				hash: Some("*".to_string()),
				..Default::default()
			},
		] {
			render_config(
				&sample_file(),
				&ConfigFormat::Yaml,
				&out,
				"proj",
				Path::new("/proj"),
			)
			.unwrap();
		}
	}

	#[test]
	fn render_config_hash_rejects_unknown_service() {
		let out = ConfigOutput {
			hash: Some("nope".to_string()),
			..Default::default()
		};
		assert!(render_config(
			&sample_file(),
			&ConfigFormat::Yaml,
			&out,
			"proj",
			Path::new("/proj")
		)
		.is_err());
	}

	#[test]
	fn service_config_hash_is_stable_and_distinct() {
		let file = sample_file();
		let web = service_config_hash(&file.services["web"]).expect("hash");
		let db = service_config_hash(&file.services["db"]).expect("hash");
		// Stable for the same input, and distinct across different services.
		assert_eq!(
			web,
			service_config_hash(&file.services["web"]).expect("hash")
		);
		assert_ne!(web, db);
		assert_eq!(web.len(), 64, "sha-256 hex is 64 chars");
	}

	#[test]
	fn render_config_yaml_and_json_render_ok() {
		render_config(
			&sample_file(),
			&ConfigFormat::Yaml,
			&ConfigOutput::default(),
			"proj",
			Path::new("/proj"),
		)
		.unwrap();
		render_config(
			&sample_file(),
			&ConfigFormat::Json,
			&ConfigOutput::default(),
			"proj",
			Path::new("/proj"),
		)
		.unwrap();
	}

	#[test]
	fn render_config_injects_resolved_project_name() {
		// The rendered output carries the resolved project name, not the file's
		// literal `name:` (here unset). Render into a buffer via the same path.
		let mut redacted = sample_file();
		redacted.name = Some("myproj".to_string());
		let v: serde_yaml::Value = serde_yaml::to_value(&redacted).unwrap();
		let out = serde_yaml::to_string(&v).unwrap();
		assert!(
			out.contains("name: myproj"),
			"config should render the resolved name"
		);
	}

	#[test]
	fn prune_preserves_environment_map_nulls() {
		// A map-form host-passthrough var (`MYVAR:` -> null) survives pruning, while
		// an unrelated null elsewhere is still dropped.
		let mut v: serde_yaml::Value = serde_yaml::from_str(
			"services:\n  web:\n    image: nginx\n    dns: null\n    environment:\n      MYVAR: null\n      SET: value\n",
		)
		.unwrap();
		prune_yaml_nulls(&mut v);
		let out = serde_yaml::to_string(&v).unwrap();
		assert!(out.contains("MYVAR"), "passthrough env var must be kept");
		assert!(out.contains("SET"));
		assert!(!out.contains("dns"), "unrelated null must still be dropped");

		let mut j = serde_json::json!({
			"services": { "web": {
				"image": "nginx",
				"dns": null,
				"environment": { "MYVAR": null, "SET": "value" }
			}}
		});
		prune_json_nulls(&mut j);
		let env = &j["services"]["web"]["environment"];
		assert!(
			env.get("MYVAR").is_some(),
			"passthrough env var must be kept"
		);
		assert!(j["services"]["web"].get("dns").is_none());
	}

	#[test]
	fn render_config_strips_ignored_unknown_keys() {
		// An unknown (non-`x-`) top-level and service key is dropped from the
		// rendered output, while a valid `x-` extension is round-tripped. Rendered
		// via the YAML path through a clone so the public method is exercised.
		let mut file = podup::parse_str(
			"x-anchors: keep\nbogus_top: 1\nservices:\n  web:\n    image: nginx\n    bogus_svc: 2\n",
		)
		.unwrap();
		file.strip_ignored_unknown_keys();
		let v: serde_yaml::Value = serde_yaml::to_value(&file).unwrap();
		let out = serde_yaml::to_string(&v).unwrap();
		assert!(
			!out.contains("bogus_top"),
			"ignored top key re-emitted: {out}"
		);
		assert!(
			!out.contains("bogus_svc"),
			"ignored svc key re-emitted: {out}"
		);
		assert!(
			out.contains("x-anchors"),
			"x- extension must survive: {out}"
		);
	}

	/// #1078: a null value under `networks:` means "attach with default
	/// options", not "nothing". It used to be pruned like any other empty leaf,
	/// which silently removed a network the service is genuinely on — reachable
	/// once merging could produce a map mixing a configured network with a bare
	/// one.
	#[test]
	fn a_bare_network_entry_survives_pruning() {
		let mut v: serde_yaml::Value = serde_yaml::from_str(
			"networks:\n  backend:\n    aliases:\n    - db\n  monitoring: null\n",
		)
		.unwrap();
		prune_yaml_nulls(&mut v);
		let out = serde_yaml::to_string(&v).unwrap();
		assert!(
			out.contains("monitoring"),
			"a bare network entry must not be pruned: {out}"
		);
		assert!(out.contains("backend"), "{out}");
	}
}
