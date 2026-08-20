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

/// `5432:5432` is the canonical accidental case: the operator writes it
/// thinking "this is for me", and the compose-spec default binds on every
/// host interface. The behavior is left as-is (docker-compose does the same)
/// but the warning makes the foot-gun visible.
#[test]
fn warns_on_short_port_without_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"5432:5432\"\n",
	);
	assert!(
		msgs.iter().any(|m| m.contains("service 'db'")
			&& m.contains("port 5432")
			&& m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// `127.0.0.1:5432:5432` is the explicit-loopback form: the bind is
/// restricted to the host, so nothing leaks to the network and no warning
/// is due.
#[test]
fn does_not_warn_on_short_port_with_loopback_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"127.0.0.1:5432:5432\"\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// `0.0.0.0:5432:5432` does the same bind as the IP-less short form — every
/// interface — but the operator typed it explicitly. Warning here would
/// train the reader to ignore this warning, so this is the case that looks
/// wrong but is deliberate.
#[test]
fn does_not_warn_on_short_port_with_explicit_all_interfaces_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"0.0.0.0:5432:5432\"\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// A service with no `ports:` field publishes nothing on the host, so the
/// warning has nothing to attach to.
#[test]
fn does_not_warn_when_service_has_no_ports() {
	let msgs = diagnostics_for("services:\n  web:\n    image: nginx\n");
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// A service with only `expose:` (no `ports:`) is reachable to peers on the
/// same compose network but not on the host — same case as no ports at all.
#[test]
fn does_not_warn_on_expose_only() {
	let msgs =
		diagnostics_for("services:\n  db:\n    image: postgres\n    expose:\n      - \"5432\"\n");
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `host_ip: 127.0.0.1` is the long-form mirror of the
/// short-loopback form: restricted to the host, no warning.
#[test]
fn does_not_warn_on_long_port_with_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 5432\n        published: 5432\n        host_ip: 127.0.0.1\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `published` but no `host_ip` is the long-form mirror of
/// `5432:5432`: bind on every interface, warning fires.
#[test]
fn warns_on_long_port_without_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 80\n        published: 8080\n",
	);
	assert!(
		msgs.iter().any(|m| m.contains("service 'db'")
			&& m.contains("port 8080")
			&& m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `published: 8080` and `host_ip: 0.0.0.0` is the long-form
/// mirror of the explicit-all-interfaces short form: explicit bind, no warning.
#[test]
fn does_not_warn_on_long_port_with_explicit_all_interfaces_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 80\n        published: 8080\n        host_ip: 0.0.0.0\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `target` but no `published` is the long-form mirror of
/// `expose:`: the port is exposed to peers but not published on the host,
/// so the warning has nothing to attach to.
#[test]
fn does_not_warn_on_long_port_without_published() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 5432\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Short form with a `/tcp` protocol suffix still lacks a host IP, so the
/// warning fires just like the bare `8080:80`.
#[test]
fn warns_on_short_port_with_protocol_suffix() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"8080:80/tcp\"\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("port 8080") && m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Short form with just a container port (`"80"`) is the short-form mirror
/// of `expose:`: not published on the host, so no warning.
#[test]
fn does_not_warn_on_short_container_only_port() {
	let msgs =
		diagnostics_for("services:\n  db:\n    image: postgres\n    ports:\n      - \"80\"\n");
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// IPv6 short form `[::1]:5432:5432` carries an explicit host IP, so the
/// bind is restricted and no warning is due.
#[test]
fn does_not_warn_on_ipv6_short_port() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"[::1]:5432:5432\"\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Two ports on the same service: one with IP, one without — only the
/// IP-less one warns.
#[test]
fn warns_only_for_ip_less_port_in_mixed_list() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"127.0.0.1:5432:5432\"\n      - \"8080:80\"\n",
	);
	let exposure_warnings: Vec<_> = msgs
		.iter()
		.filter(|m| m.contains("every interface"))
		.collect();
	assert_eq!(
		exposure_warnings.len(),
		1,
		"expected exactly one exposure warning; got: {exposure_warnings:?}"
	);
	assert!(
		exposure_warnings[0].contains("port 8080"),
		"warning should name the IP-less port; got: {:?}",
		exposure_warnings[0]
	);
}
