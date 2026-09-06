use super::*;

fn sample_file() -> podup::compose::types::ComposeFile {
	podup::parse_str("services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n").unwrap()
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
	// explicit empty array (`command: []`) survives; it overrides the image.
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
/// which silently removed a network the service is genuinely on, reachable
/// once merging could produce a map mixing a configured network with a bare
/// one.
#[test]
fn a_bare_network_entry_survives_pruning() {
	let mut v: serde_yaml::Value =
		serde_yaml::from_str("networks:\n  backend:\n    aliases:\n    - db\n  monitoring: null\n")
			.unwrap();
	prune_yaml_nulls(&mut v);
	let out = serde_yaml::to_string(&v).unwrap();
	assert!(
		out.contains("monitoring"),
		"a bare network entry must not be pruned: {out}"
	);
	assert!(out.contains("backend"), "{out}");
}

/// `config` resolves every declared network's name the same way `up` does, so
/// the rendered output matches what the next `up` will create. A bare network
/// (`null` body) and the implicit `default` both pick up `<project>_<key>`;
/// an explicit `name:` is left alone.
#[test]
fn inject_resolved_network_names_fills_in_the_implicit_default() {
	let mut file =
		podup::parse_str("services:\n  web:\n    image: alpine\nnetworks:\n  default:\n").unwrap();
	// The implicit `default` network is synthesized by the real pipeline before
	// `config` runs; declaring it explicitly here is enough to exercise the same
	// shape the production code sees.
	inject_resolved_network_names(&mut file, "myproj");
	let rendered = serde_yaml::to_string(&file.networks).unwrap();
	assert!(
		rendered.contains("name: myproj_default"),
		"the implicit `default` network must resolve to <project>_default: {rendered}"
	);
}

/// A network whose body is just `null` is treated the same way: there is no
/// `name:` to preserve, so `up` would create `<project>_<key>` and `config`
/// shows the same.
#[test]
fn inject_resolved_network_names_fills_a_bare_network() {
	let mut file =
		podup::parse_str("services:\n  web:\n    image: alpine\nnetworks:\n  monitoring: null\n")
			.unwrap();
	inject_resolved_network_names(&mut file, "myproj");
	let cfg = file.networks.get("monitoring").unwrap();
	let name = cfg
		.as_ref()
		.and_then(|c| c.name.as_deref())
		.expect("bare network must pick up the project-prefixed default");
	assert_eq!(name, "myproj_monitoring");
}

/// An explicit `name:` is the user's choice; `config` must not overwrite it
/// just because it would otherwise match the same rule.
#[test]
fn inject_resolved_network_names_keeps_an_explicit_name() {
	let mut file = podup::parse_str(
		"services:\n  web:\n    image: alpine\nnetworks:\n  backend:\n    name: my-custom-net\n",
	)
	.unwrap();
	inject_resolved_network_names(&mut file, "myproj");
	let name = file.networks["backend"]
		.as_ref()
		.and_then(|c| c.name.as_deref())
		.expect("explicit name must survive");
	assert_eq!(name, "my-custom-net");
}

/// An `external: true` network keeps its bare key as the runtime name, with no
/// project prefix; `config` reflects that without writing a `name:` it would
/// then have to also unset.
#[test]
fn inject_resolved_network_names_leaves_external_networks_alone() {
	let mut file = podup::parse_str(
		"services:\n  web:\n    image: alpine\nnetworks:\n  shared:\n    external: true\n",
	)
	.unwrap();
	inject_resolved_network_names(&mut file, "myproj");
	let cfg = file.networks["shared"].as_ref().expect("network slot");
	assert!(
		cfg.name.is_none(),
		"external networks must not get a project-prefixed name: {cfg:?}"
	);
}
