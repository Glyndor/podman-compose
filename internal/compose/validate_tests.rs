use super::*;
use crate::{parse_str, parse_str_raw};

fn file(yaml: &str) -> ComposeFile {
	parse_str_raw(yaml).unwrap()
}

fn validate_str(yaml: &str) -> crate::error::Result<()> {
	let mut file: ComposeFile = parse_str(yaml).unwrap();
	// Mirror the CLI: synthesize the implicit default network before validating
	// so a bare service is not flagged as referencing an undefined network.
	crate::compose::normalize_default_network(&mut file);
	super::validate(&file)
}

// validate_config (the `config` subcommand)

#[test]
fn empty_services_is_rejected() {
	let err = validate_config(&file("services: {}\n")).unwrap_err();
	assert!(format!("{err}").contains("no services"));
	// A file with no `services:` key at all is equally rejected.
	assert!(validate_config(&ComposeFile::default()).is_err());
}

#[test]
fn missing_image_and_build_is_rejected() {
	let err = validate_config(&file("services:\n  web:\n    ports: ['80:80']\n")).unwrap_err();
	assert!(matches!(err, ComposeError::NoImageOrBuild(_)));
}

#[test]
fn valid_minimal_file_passes() {
	validate_config(&file("services:\n  web:\n    image: nginx\n")).unwrap();
}

#[test]
fn out_of_range_port_is_rejected() {
	let err = validate_config(&file(
		"services:\n  web:\n    image: nginx\n    ports: ['99999:80']\n",
	))
	.unwrap_err();
	assert!(matches!(err, ComposeError::InvalidPort(_)));
}

#[test]
fn zero_port_is_rejected() {
	let err = validate_config(&file(
		"services:\n  web:\n    image: nginx\n    ports: ['0:80']\n",
	))
	.unwrap_err();
	assert!(matches!(err, ComposeError::InvalidPort(_)));
}

#[test]
fn undefined_named_volume_is_rejected() {
	let err = validate_config(&file(
		"services:\n  web:\n    image: nginx\n    volumes: ['data:/x']\n",
	))
	.unwrap_err();
	assert!(format!("{err}").contains("undefined volume 'data'"));
}

#[test]
fn declared_named_volume_passes() {
	validate_config(&file(
		"services:\n  web:\n    image: nginx\n    volumes: ['data:/x']\nvolumes:\n  data:\n",
	))
	.unwrap();
}

#[test]
fn bind_and_anonymous_volumes_are_not_flagged() {
	// Host-path binds and anonymous volumes carry no top-level declaration.
	validate_config(&file(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - ./host:/x\n      - /abs:/y\n      - /data\n",
	))
	.unwrap();
}

#[test]
fn undefined_network_is_rejected() {
	let err = validate_config(&file(
		"services:\n  web:\n    image: nginx\n    networks: [backend]\n",
	))
	.unwrap_err();
	assert!(format!("{err}").contains("undefined network 'backend'"));
}

#[test]
fn declared_network_passes() {
	validate_config(&file(
		"services:\n  web:\n    image: nginx\n    networks: [backend]\nnetworks:\n  backend:\n",
	))
	.unwrap();
}

#[test]
fn invalid_service_name_is_rejected() {
	let err = validate_config(&file("services:\n  'bad name':\n    image: nginx\n")).unwrap_err();
	assert!(format!("{err}").contains("service name"));
}

#[test]
fn dependency_cycle_is_rejected() {
	let err = validate_config(&file(
		"services:\n  a:\n    image: x\n    depends_on: [b]\n  b:\n    image: y\n    depends_on: [a]\n",
	))
	.unwrap_err();
	assert!(matches!(err, ComposeError::CircularDependency(_)));
}

#[test]
fn dangling_required_dependency_is_rejected() {
	let err = validate_config(&file(
		"services:\n  web:\n    image: nginx\n    depends_on: [ghost]\n",
	))
	.unwrap_err();
	assert!(matches!(err, ComposeError::ServiceNotFound(_)));
}

// validate (the post-parse semantic pass)

#[test]
fn network_mode_with_networks_is_rejected() {
	let yaml = "services:\n  web:\n    image: x\n    network_mode: host\n    networks: [front]\nnetworks:\n  front:\n";
	let err = validate_str(yaml).unwrap_err();
	assert!(err.to_string().contains("mutually exclusive"), "got: {err}");
}

#[test]
fn network_mode_alone_is_accepted() {
	let yaml = "services:\n  web:\n    image: x\n    network_mode: host\n";
	assert!(validate_str(yaml).is_ok());
}

#[test]
fn undefined_network_reference_is_rejected() {
	let yaml = "services:\n  web:\n    image: x\n    networks: [missing]\n";
	let err = validate_str(yaml).unwrap_err();
	assert!(
		err.to_string().contains("undefined network 'missing'"),
		"got: {err}"
	);
}

#[test]
fn declared_network_reference_is_accepted() {
	let yaml = "services:\n  web:\n    image: x\n    networks: [front]\nnetworks:\n  front:\n";
	assert!(validate_str(yaml).is_ok());
}

#[test]
fn bare_service_default_network_is_accepted() {
	// No networks declared at all: the synthesized `default` must satisfy the
	// reference check, not trip it.
	let yaml = "services:\n  web:\n    image: x\n";
	assert!(validate_str(yaml).is_ok());
}

#[test]
fn explicit_default_network_reference_is_accepted() {
	// `default` is the implicit project network; referencing it without a
	// top-level entry must not be flagged as undefined.
	let yaml = "services:\n  web:\n    image: x\n    networks: [default]\n";
	assert!(validate_str(yaml).is_ok());
}

#[test]
fn external_network_with_internal_is_rejected() {
	let yaml = "services:\n  web:\n    image: x\n    networks: [ext]\nnetworks:\n  ext:\n    external: true\n    internal: true\n";
	let err = validate_str(yaml).unwrap_err();
	let msg = err.to_string();
	assert!(msg.contains("external"), "got: {msg}");
	assert!(msg.contains("internal"), "got: {msg}");
}

#[test]
fn external_network_with_ipam_is_rejected() {
	let yaml = "services:\n  web:\n    image: x\n    networks: [ext]\nnetworks:\n  ext:\n    external: true\n    ipam:\n      config:\n        - subnet: 10.0.0.0/24\n";
	let err = validate_str(yaml).unwrap_err();
	assert!(err.to_string().contains("ipam"), "got: {err}");
}

#[test]
fn plain_external_network_is_accepted() {
	let yaml = "services:\n  web:\n    image: x\n    networks: [ext]\nnetworks:\n  ext:\n    external: true\n";
	assert!(validate_str(yaml).is_ok());
}

#[test]
fn external_network_with_name_only_is_accepted() {
	let yaml = "services:\n  web:\n    image: x\n    networks: [ext]\nnetworks:\n  ext:\n    external: true\n    name: shared_net\n";
	assert!(validate_str(yaml).is_ok());
}

#[test]
fn out_of_range_short_port_is_rejected() {
	let yaml = "services:\n  web:\n    image: x\n    ports: ['99999:80']\n";
	assert!(validate_str(yaml).is_err());
}

#[test]
fn invalid_port_protocol_is_rejected() {
	let yaml = "services:\n  web:\n    image: x\n    ports: ['80/banana']\n";
	assert!(validate_str(yaml).is_err());
}
