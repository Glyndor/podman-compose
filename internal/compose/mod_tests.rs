use super::*;

// parse_str_raw

#[test]
fn is_stdin_matches_only_the_dash_sentinel() {
	assert!(is_stdin(Path::new("-")));
	assert!(!is_stdin(Path::new("docker-compose.yml")));
	assert!(!is_stdin(Path::new("./-")));
	assert!(!is_stdin(Path::new("a-b")));
}

#[test]
fn parse_str_raw_minimal_service() {
	let yaml = "services:\n  web:\n    image: nginx\n";
	let file = parse_str_raw(yaml).unwrap();
	assert!(file.services.contains_key("web"));
	assert_eq!(file.services["web"].image.as_deref(), Some("nginx"));
}

#[test]
fn collect_diagnostics_surfaces_unknown_keys() {
	// The public helper lets library consumers see the same warnings the CLI
	// prints; parse_file itself stays quiet.
	let file = parse_str_raw("services:\n  web:\n    image: nginx\n    enviroment:\n      - A=1\n")
		.unwrap();
	let diags = collect_diagnostics(&file);
	assert!(
		diags.iter().any(|d| d.contains("enviroment")),
		"expected an unknown-key diagnostic, got {diags:?}"
	);
}

#[test]
fn parse_str_raw_invalid_yaml_is_error() {
	assert!(parse_str_raw(": : :").is_err());
}

// unknown-key capture / warning

#[test]
fn unknown_service_key_is_captured_not_dropped() {
	// A typo'd key lands in `unknown` instead of vanishing silently.
	let yaml = "services:\n  web:\n    image: nginx\n    enviroment:\n      - A=1\n";
	let file = parse_str_raw(yaml).unwrap();
	assert!(file.services["web"].unknown.contains_key("enviroment"));
	assert!(file.services["web"].environment.is_empty());
}

#[test]
fn known_service_keys_do_not_land_in_unknown() {
	let yaml = "services:\n  web:\n    image: nginx\n    environment:\n      - A=1\n";
	let file = parse_str_raw(yaml).unwrap();
	assert!(file.services["web"].unknown.is_empty());
}

// Multi-file `-f` override merge

#[test]
fn merge_override_adds_models_and_override_wins() {
	use crate::compose::types::ModelConfig;
	let model = |m: &str| ModelConfig {
		model: Some(m.to_string()),
		..Default::default()
	};
	let mut target = ComposeFile::default();
	target.models.insert("llm".to_string(), model("base/m"));
	let mut other = ComposeFile::default();
	other.models.insert("llm".to_string(), model("over/m"));
	other.models.insert("extra".to_string(), model("e/m"));
	merge_override(&mut target, other, &tags::Directives::new());
	// Override file wins on conflict; the override-only model is added.
	assert_eq!(target.models["llm"].model.as_deref(), Some("over/m"));
	assert_eq!(target.models["extra"].model.as_deref(), Some("e/m"));
}

#[test]
fn merge_override_unions_top_level_resource_maps() {
	use crate::compose::types::{ConfigConfig, NetworkConfig, SecretConfig, VolumeConfig};
	let mut target = ComposeFile::default();
	target
		.volumes
		.insert("data".to_string(), Some(VolumeConfig::default()));
	target
		.networks
		.insert("net".to_string(), Some(NetworkConfig::default()));
	target.secrets.insert(
		"tok".to_string(),
		SecretConfig {
			file: Some("base.txt".to_string()),
			..Default::default()
		},
	);
	target
		.configs
		.insert("cfg".to_string(), ConfigConfig::default());

	let mut other = ComposeFile::default();
	// An override-only volume/network/config is added; an overlapping secret is
	// replaced by the override file's definition.
	other
		.volumes
		.insert("cache".to_string(), Some(VolumeConfig::default()));
	other
		.networks
		.insert("backend".to_string(), Some(NetworkConfig::default()));
	other.secrets.insert(
		"tok".to_string(),
		SecretConfig {
			file: Some("override.txt".to_string()),
			..Default::default()
		},
	);
	other
		.configs
		.insert("extra".to_string(), ConfigConfig::default());

	merge_override(&mut target, other, &tags::Directives::new());

	assert!(target.volumes.contains_key("data"));
	assert!(target.volumes.contains_key("cache"));
	assert!(target.networks.contains_key("net"));
	assert!(target.networks.contains_key("backend"));
	assert_eq!(
		target.secrets["tok"].file.as_deref(),
		Some("override.txt"),
		"the override file's secret definition must win"
	);
	assert!(target.configs.contains_key("cfg"));
	assert!(target.configs.contains_key("extra"));
}

// YAML merge keys (<<)

#[test]
fn yaml_merge_key_fills_missing_fields() {
	let yaml = "x-defaults: &defaults\n  image: nginx\n  restart: always\nservices:\n  web:\n    <<: *defaults\n    ports: ['80:80']\n";
	let file = parse_str_raw(yaml).unwrap();
	assert_eq!(file.services["web"].image.as_deref(), Some("nginx"));
}

// Default-network synthesis (#417)

#[test]
fn normalize_attaches_bare_service_to_default_network() {
	let mut file = parse_str("services:\n  web:\n    image: nginx\n").unwrap();
	normalize_default_network(&mut file);
	assert!(file.networks.contains_key("default"));
	assert_eq!(file.services["web"].networks.names(), vec!["default"]);
}

#[test]
fn normalize_leaves_service_with_explicit_networks_untouched() {
	let mut file = parse_str(
		"services:\n  web:\n    image: nginx\n    networks: [front]\nnetworks:\n  front:\n",
	)
	.unwrap();
	normalize_default_network(&mut file);
	assert_eq!(file.services["web"].networks.names(), vec!["front"]);
	// No default network is synthesized when nothing needs it.
	assert!(!file.networks.contains_key("default"));
}

#[test]
fn normalize_skips_service_with_network_mode() {
	let mut file =
		parse_str("services:\n  web:\n    image: nginx\n    network_mode: host\n").unwrap();
	normalize_default_network(&mut file);
	assert!(file.services["web"].networks.names().is_empty());
	assert!(!file.networks.contains_key("default"));
}

#[test]
fn normalize_respects_explicit_default_network_config() {
	let mut file = parse_str(
		"services:\n  web:\n    image: nginx\nnetworks:\n  default:\n    driver: bridge\n",
	)
	.unwrap();
	normalize_default_network(&mut file);
	// The user-defined `default` config is kept, not overwritten with None.
	assert!(file.networks["default"].is_some());
	assert_eq!(file.services["web"].networks.names(), vec!["default"]);
}
