//! Parse-time diagnostics unit tests (split from mod.rs to stay under the
//! per-file line limit).

use crate::parse_str;

fn diagnostics_for(yaml: &str) -> Vec<String> {
	let file = parse_str(yaml).unwrap();
	super::collect(&file)
}

#[test]
fn warns_on_unknown_top_level_key_but_not_x_extension() {
	let msgs = diagnostics_for(
		"x-anchors: ok\nservies:\n  typo: 1\nservices:\n  web:\n    image: nginx\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("unknown top-level key 'servies'")),
		"got: {msgs:?}"
	);
	assert!(!msgs.iter().any(|m| m.contains("x-anchors")));
}

#[test]
fn warns_on_unknown_service_key_but_not_x_extension() {
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\n    enviroment:\n      A: 1\n    x-meta: ok\n",
	);
	assert!(msgs.iter().any(|m| m.contains("unknown key 'enviroment'")));
	assert!(!msgs.iter().any(|m| m.contains("x-meta")));
}

#[test]
fn warns_on_unknown_develop_watch_key() {
	// An unrecognized key inside a `develop.watch[*]` rule is surfaced with the
	// indexed context, but an `x-` extension key on the same rule is left alone.
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\n    develop:\n      watch:\n        - path: ./src\n          action: sync\n          target: /app\n          bogus_key: 1\n          x-note: ok\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("develop.watch[0]") && m.contains("bogus_key")),
		"got: {msgs:?}"
	);
	assert!(!msgs.iter().any(|m| m.contains("x-note")));
}

#[test]
fn nested_x_extension_key_is_not_flagged() {
	// An `x-` key inside a modeled sub-object (here, healthcheck) is a valid
	// extension and must not produce an "unknown key" warning.
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\n    healthcheck:\n      test: [\"CMD\", \"true\"]\n      x-custom: ok\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("x-custom")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_windows_only_cpu_fields() {
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\n    cpu_count: 2\n    cpu_percent: 50\n",
	);
	assert!(msgs.iter().any(|m| m.contains("cpu_count")));
	assert!(msgs.iter().any(|m| m.contains("cpu_percent")));
}

#[test]
fn warns_on_unmapped_build_fields() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    build:\n      context: .\n      privileged: true\n      isolation: chroot\n",
		);
	assert!(msgs.iter().any(|m| m.contains("build.privileged")));
	assert!(msgs.iter().any(|m| m.contains("build.isolation")));
}

#[test]
fn warns_on_network_enable_ipv4() {
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\nnetworks:\n  net:\n    enable_ipv4: false\n",
	);
	assert!(msgs.iter().any(|m| m.contains("enable_ipv4")));
}

#[test]
fn warns_on_unknown_key_in_healthcheck() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    healthcheck:\n      test: [\"CMD\", \"true\"]\n      retires: 3\n",
		);
	assert!(
		msgs.iter()
			.any(|m| m.contains("healthcheck") && m.contains("retires")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_env_file_format() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    env_file:\n      - path: ./a.env\n        format: raw\n",
		);
	assert!(
		msgs.iter()
			.any(|m| m.contains("env_file format") && m.contains("dotenv")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_build_ssh() {
	let msgs = diagnostics_for(
		"services:\n  web:\n    build:\n      context: .\n      ssh:\n        - default\n",
	);
	assert!(
		msgs.iter().any(|m| m.contains("build.ssh")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_unknown_key_in_network_and_ipam() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\nnetworks:\n  net:\n    drivr: bridge\n    ipam:\n      confg: []\n",
		);
	assert!(msgs
		.iter()
		.any(|m| m.contains("network 'net'") && m.contains("drivr")));
	assert!(msgs
		.iter()
		.any(|m| m.contains("ipam") && m.contains("confg")));
}

#[test]
fn warns_on_unknown_key_in_volume() {
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\nvolumes:\n  data:\n    externl: true\n",
	);
	assert!(msgs
		.iter()
		.any(|m| m.contains("volume 'data'") && m.contains("externl")));
}

#[test]
fn warns_on_service_network_gw_priority() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    networks:\n      net:\n        gw_priority: 10\nnetworks:\n  net:\n",
		);
	assert!(
		msgs.iter().any(|m| m.contains("gw_priority")),
		"got: {msgs:?}"
	);
}

#[test]
fn file_secret_produces_no_diagnostics() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    file: ./tok.txt\n",
		);
	assert!(msgs.is_empty(), "unexpected diagnostics: {msgs:?}");
}

#[test]
fn clean_file_produces_no_diagnostics() {
	let msgs = diagnostics_for("services:\n  web:\n    image: nginx\n    cpu_shares: 512\n");
	assert!(msgs.is_empty(), "unexpected diagnostics: {msgs:?}");
}

#[test]
fn attach_is_honored_no_warning() {
	// `attach: false` is honored (it suppresses the service's `up` log streaming,
	// matching Compose), so it must NOT produce an "ignored field" diagnostic.
	let msgs = diagnostics_for("services:\n  web:\n    image: nginx\n    attach: false\n");
	assert!(
		!msgs.iter().any(|m| m.contains("attach")),
		"attach should not be flagged as ignored: {msgs:?}"
	);
}

#[test]
fn warns_on_long_port_mode() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    ports:\n      - target: 80\n        published: 8080\n        mode: host\n",
		);
	assert!(
		msgs.iter().any(|m| m.contains("port mode 'host'")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_per_mount_driver_config() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    volumes:\n      - type: volume\n        source: data\n        target: /data\n        volume:\n          driver_config:\n            name: local\nvolumes:\n  data:\n",
		);
	assert!(
		msgs.iter().any(|m| m.contains("per-mount driver_config")),
		"got: {msgs:?}"
	);
}

#[test]
fn does_not_warn_on_interface_name() {
	// interface_name IS forwarded to Podman (PerNetworkOptions.interface_name),
	// so it must not produce a "not forwarded / ignored" warning.
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    networks:\n      net:\n        interface_name: eth9\nnetworks:\n  net:\n",
		);
	assert!(
		!msgs.iter().any(|m| m.contains("interface_name")),
		"interface_name should not be reported as ignored; got: {msgs:?}"
	);
}

#[test]
fn warns_on_non_external_secret_driver() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    driver: vault\n",
		);
	assert!(
		msgs.iter().any(|m| m.contains("secret 'tok': driver")),
		"got: {msgs:?}"
	);
}

#[test]
fn external_secret_driver_produces_no_diagnostic() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n    driver: vault\n",
		);
	assert!(
		!msgs.iter().any(|m| m.contains("driver")),
		"unexpected: {msgs:?}"
	);
}

#[test]
fn warns_on_remaining_unmapped_build_fields() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    build:\n      context: .\n      ulimits:\n        nofile: 1024\n      entitlements: [\"security.insecure\"]\n      provenance: true\n      sbom: true\n",
		);
	for field in ["build.entitlements", "build.provenance", "build.sbom"] {
		assert!(
			msgs.iter().any(|m| m.contains(field)),
			"missing {field}; got: {msgs:?}"
		);
	}
	// `build.ulimits` was on this list, wrongly: the libpod build endpoint takes
	// a `ulimits` parameter and applies it. Measured on podman 5.7.0 — the same
	// build saw `ulimit -n` of 524288 without it and 1234 with it. Reporting it
	// as unmapped now would send the reader looking for a workaround that is not
	// needed.
	assert!(
		!msgs.iter().any(|m| m.contains("build.ulimits")),
		"build.ulimits is mapped now and must not be reported as ignored; got: {msgs:?}"
	);
}

#[test]
fn warns_on_secret_template_driver() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    template_driver: golang\n",
		);
	assert!(
		msgs.iter()
			.any(|m| m.contains("secret 'tok': template_driver")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_non_external_config_driver_and_template_driver() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\nconfigs:\n  conf:\n    driver: vault\n    template_driver: golang\n",
		);
	assert!(
		msgs.iter().any(|m| m.contains("config 'conf': driver")),
		"got: {msgs:?}"
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("config 'conf': template_driver")),
		"got: {msgs:?}"
	);
}

#[test]
fn external_config_driver_produces_no_diagnostic() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\nconfigs:\n  conf:\n    external: true\n    driver: vault\n",
		);
	assert!(
		!msgs.iter().any(|m| m.contains("driver")),
		"unexpected: {msgs:?}"
	);
}

#[test]
fn warns_on_credential_spec_not_honored() {
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\n    credential_spec:\n      config: my-spec\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("credential_spec") && m.contains("not honored")),
		"got: {msgs:?}"
	);
	// The recognized key must not also produce a generic "unknown key" warning.
	assert!(
		!msgs
			.iter()
			.any(|m| m.contains("unknown key 'credential_spec'")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_service_isolation_not_honored() {
	let msgs = diagnostics_for("services:\n  web:\n    image: nginx\n    isolation: hyperv\n");
	assert!(
		msgs.iter()
			.any(|m| m.contains("isolation") && m.contains("not honored")),
		"got: {msgs:?}"
	);
	assert!(!msgs.iter().any(|m| m.contains("unknown key 'isolation'")));
}

#[test]
fn warns_on_provider_not_honored() {
	let msgs = diagnostics_for("services:\n  db:\n    provider:\n      type: awesomecloud\n");
	assert!(
		msgs.iter()
			.any(|m| m.contains("provider") && m.contains("not honored")),
		"got: {msgs:?}"
	);
	assert!(!msgs.iter().any(|m| m.contains("unknown key 'provider'")));
}

#[test]
fn warns_on_use_api_socket_not_honored() {
	let msgs = diagnostics_for("services:\n  web:\n    image: nginx\n    use_api_socket: true\n");
	assert!(
		msgs.iter()
			.any(|m| m.contains("use_api_socket") && m.contains("not honored")),
		"got: {msgs:?}"
	);
	assert!(!msgs
		.iter()
		.any(|m| m.contains("unknown key 'use_api_socket'")));
}

#[test]
fn warns_on_ipam_aux_addresses() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\nnetworks:\n  net:\n    ipam:\n      config:\n        - subnet: 10.0.0.0/24\n          aux_addresses:\n            host1: 10.0.0.5\n",
		);
	assert!(
		msgs.iter().any(|m| m.contains("aux_addresses")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_restart_policy_delay_and_window() {
	let msgs = diagnostics_for(
			"services:\n  web:\n    image: nginx\n    deploy:\n      restart_policy:\n        condition: on-failure\n        delay: 5s\n        window: 120s\n",
		);
	assert!(
		msgs.iter().any(|m| m.contains("restart_policy.delay")),
		"got: {msgs:?}"
	);
	assert!(
		msgs.iter().any(|m| m.contains("restart_policy.window")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_top_level_models_not_honored() {
	let msgs = diagnostics_for(
		"services:\n  web:\n    image: nginx\nmodels:\n  llm:\n    model: ai/model\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("model 'llm'") && m.contains("not honored")),
		"got: {msgs:?}"
	);
	// `models` is now a recognized top-level element, not an unknown key.
	assert!(
		!msgs
			.iter()
			.any(|m| m.contains("unknown top-level key 'models'")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_typo_inside_provider_and_models() {
	let msgs = diagnostics_for(
			"services:\n  db:\n    provider:\n      type: cloud\n      optoins: {}\nmodels:\n  llm:\n    modle: ai/model\n",
		);
	assert!(
		msgs.iter()
			.any(|m| m.contains("provider") && m.contains("optoins")),
		"got: {msgs:?}"
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("model 'llm'") && m.contains("modle")),
		"got: {msgs:?}"
	);
}

#[cfg(test)]
#[path = "port_exposure_tests.rs"]
mod port_exposure;
