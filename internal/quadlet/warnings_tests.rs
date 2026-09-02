use crate::parse_str;
use crate::quadlet::generate_at;

#[test]
fn warns_for_every_unmapped_field() {
	let yaml = r#"
services:
  everything:
    image: app:1.0
    build: .
    scale: 3
    network_mode: "bridge:custom"
    volumes_from:
      - other
    profiles:
      - debug
    healthcheck:
      test: ["CMD", "true"]
    secrets:
      - my_secret
    configs:
      - my_config
secrets:
  my_secret:
    file: ./s.txt
configs:
  my_config:
    file: ./c.txt
"#;
	let file = parse_str(yaml).unwrap();
	let warnings = generate_at(&file, "proj", std::path::Path::new("/srv/app")).warnings;
	let joined = warnings.join("\n");

	for field in [
		"scale/replicas",
		"configs",
		"volumes_from",
		"network_mode",
		"profiles",
	] {
		assert!(
			joined.contains(field),
			"missing warning for {field}; got:\n{joined}"
		);
	}
	// secrets are now mapped to Secret=, so they must NOT warn.
	assert!(
		!joined.contains("secrets"),
		"secrets should be mapped, not warned; got:\n{joined}"
	);
	// privileged is now mapped to PodmanArgs=--privileged, not warned.
	assert!(
		!joined.contains("privileged"),
		"privileged should be mapped, not warned; got:\n{joined}"
	);
}

#[test]
fn service_and_container_network_modes_are_mapped_not_warned() {
	// `service:X`/`container:X` map to `Network=X.container`, so they must not
	// warn; only other unmapped modes (bridge:, custom, …) warn.
	for mode in ["service:db", "container:other"] {
		let yaml = format!("services:\n  s:\n    image: x\n    network_mode: \"{mode}\"\n");
		let file = parse_str(&yaml).unwrap();
		let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
			.warnings
			.join("\n");
		assert!(
			!joined.contains("network_mode"),
			"{mode} should be mapped, not warned; got:\n{joined}"
		);
	}
}

#[test]
fn warns_for_silently_dropped_runtime_fields() {
	let yaml = r#"
services:
  s:
    image: x
    ipc: host
    pid: host
    uts: host
    cgroup: private
    cgroup_parent: /sys/fs/cgroup/p
    runtime: crun
    tty: true
    stdin_open: true
    mac_address: "02:42:ac:11:00:02"
    memswap_limit: 1g
    mem_reservation: 256m
    oom_kill_disable: true
    oom_score_adj: -500
    label_file:
      - ./labels.env
    blkio_config:
      weight: 300
"#;
	let file = parse_str(yaml).unwrap();
	let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
		.warnings
		.join("\n");
	for field in [
		"ipc",
		"pid",
		"uts",
		"cgroup",
		"cgroup_parent",
		"runtime",
		"tty",
		"stdin_open",
		"mac_address",
		"memswap_limit",
		"mem_reservation",
		"oom_kill_disable",
		"oom_score_adj",
		"label_file",
		"blkio_config",
	] {
		assert!(
			joined.contains(field),
			"missing warning for {field}; got:\n{joined}"
		);
	}
}

#[test]
fn warns_for_additional_silently_dropped_fields() {
	let yaml = r#"
services:
  s:
    image: x
    gpus: all
    platform: linux/arm64
    domainname: example.internal
    links:
      - other
    external_links:
      - ext:alias
    device_cgroup_rules:
      - "c 1:3 rwm"
    storage_opt:
      size: 10G
    mem_swappiness: 50
    cpu_rt_runtime: 95000
    cpu_rt_period: 1000000
"#;
	let file = parse_str(yaml).unwrap();
	let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
		.warnings
		.join("\n");
	for field in [
		"gpus",
		"platform",
		"domainname",
		"links",
		"external_links",
		"device_cgroup_rules",
		"storage_opt",
		"mem_swappiness",
		"cpu_rt_runtime",
		"cpu_rt_period",
	] {
		assert!(
			joined.contains(field),
			"missing warning for {field}; got:\n{joined}"
		);
	}
}

#[test]
fn security_opt_mask_and_unmask_map_to_keys_not_warned() {
	let yaml = r#"
services:
  s:
    image: x
    security_opt:
      - "mask=/proc/kcore:/proc/timer_list"
      - "unmask=ALL"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let contents = out
		.units
		.iter()
		.map(|u| u.contents.as_str())
		.collect::<String>();
	assert!(
		contents.contains("Mask=/proc/kcore:/proc/timer_list"),
		"missing Mask= key; got:\n{contents}"
	);
	assert!(
		contents.contains("Unmask=ALL"),
		"missing Unmask= key; got:\n{contents}"
	);
	assert!(
		!out.warnings.iter().any(|w| w.contains("security_opt")),
		"mask/unmask should be mapped, not warned; got:\n{:?}",
		out.warnings
	);
}

#[test]
fn warns_when_static_ip_set_on_multiple_networks() {
	let yaml = r#"
services:
  s:
    image: x
    networks:
      a:
        ipv4_address: 10.0.0.2
      b:
        ipv4_address: 10.0.1.2
networks:
  a:
  b:
"#;
	let file = parse_str(yaml).unwrap();
	let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
		.warnings
		.join("\n");
	assert!(
		joined.contains("ipv4_address/ipv6_address"),
		"missing multi-network static-IP warning; got:\n{joined}"
	);
}

#[test]
fn warns_for_parsed_but_unmapped_service_fields() {
	// These eight fields are parsed for fidelity but have no Quadlet mapping;
	// they must each warn so nothing is silently dropped from the export.
	let yaml = r#"
services:
  s:
    image: x
    cpu_count: 2
    cpu_percent: 50
    attach: false
    develop:
      watch:
        - path: ./src
          action: sync
          target: /app
    credential_spec:
      file: cred.json
    isolation: process
    provider:
      type: terraform
    use_api_socket: true
"#;
	let file = parse_str(yaml).unwrap();
	let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
		.warnings
		.join("\n");
	for field in [
		"cpu_count",
		"cpu_percent",
		"attach",
		"develop",
		"credential_spec",
		"isolation",
		"provider",
		"use_api_socket",
	] {
		assert!(
			joined.contains(field),
			"missing warning for {field}; got:\n{joined}"
		);
	}
}

#[test]
fn clean_service_warns_about_nothing() {
	let yaml = r#"
services:
  web:
    image: nginx:1.27
"#;
	let file = parse_str(yaml).unwrap();
	assert!(generate_at(&file, "proj", std::path::Path::new("/srv/app"))
		.warnings
		.is_empty());
}
