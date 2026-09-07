use super::unit_named;
use crate::parse_str;
use crate::quadlet::generate_at;

#[test]
fn external_network_and_volume_emit_no_unit_and_use_bare_name() {
	let yaml = r#"
services:
  web:
    image: nginx
    networks:
      - extnet
    volumes:
      - extvol:/data
networks:
  extnet:
    external: true
volumes:
  extvol:
    external: true
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	// No unit is generated for external resources.
	assert!(!out
		.units
		.iter()
		.any(|u| u.filename == "proj-extnet.network"));
	assert!(!out.units.iter().any(|u| u.filename == "proj-extvol.volume"));
	// The container references them by their existing name, not `.network`/`.volume`.
	let c = &unit_named(&out, "proj-web.container").contents;
	assert!(c.contains("Network=extnet"));
	assert!(!c.contains("Network=proj-extnet.network"));
	assert!(c.contains("Volume=extvol:/data"));
	assert!(!c.contains("extvol.volume"));
}

#[test]
fn long_form_bind_selinux_and_propagation_preserved() {
	let yaml = r#"
services:
  s:
    image: x
    volumes:
      - type: bind
        source: /host/data
        target: /data
        bind:
          selinux: z
          propagation: rshared
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("Volume=/host/data:/data:z,rshared"),
		"selinux/propagation must be preserved; got:\n{c}"
	);
}

#[test]
fn network_and_volume_units_carry_config_keys() {
	let yaml = r#"
services:
  s:
    image: x
networks:
  net:
    driver: bridge
    internal: true
    enable_ipv6: true
    labels:
      tier: net
volumes:
  vol:
    driver: local
    labels:
      tier: vol
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let net = &unit_named(&out, "p-net.network").contents;
	assert!(net.contains("Driver=bridge"));
	assert!(net.contains("Internal=true"));
	assert!(net.contains("IPv6=true"));
	assert!(net.contains("Label=tier=net"));
	let vol = &unit_named(&out, "p-vol.volume").contents;
	assert!(vol.contains("Driver=local"));
	assert!(vol.contains("Label=tier=vol"));
}

#[test]
fn network_unit_emits_ipam_pools_options_and_custom_name() {
	let yaml = r#"
services:
  s:
    image: x
networks:
  net:
    name: custom-net
    driver_opts:
      mtu: "9000"
      com.docker.network.bridge.name: br0
    ipam:
      driver: host-local
      config:
        - subnet: 10.7.0.0/16
          gateway: 10.7.0.1
          ip_range: 10.7.0.128/25
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let net = &unit_named(&out, "p-net.network").contents;
	// A custom name overrides the project-prefixed default.
	assert!(net.contains("NetworkName=custom-net"), "in:\n{net}");
	assert!(!net.contains("NetworkName=p_net"), "in:\n{net}");
	assert!(net.contains("IPAMDriver=host-local"), "in:\n{net}");
	assert!(net.contains("Subnet=10.7.0.0/16"), "in:\n{net}");
	assert!(net.contains("Gateway=10.7.0.1"), "in:\n{net}");
	assert!(net.contains("IPRange=10.7.0.128/25"), "in:\n{net}");
	// Each driver option is its own Options= line (Quadlet maps one Options= to
	// one `--opt`); a comma-joined value would be a single malformed option.
	assert!(net.contains("Options=mtu=9000"), "in:\n{net}");
	assert!(
		net.contains("Options=com.docker.network.bridge.name=br0"),
		"in:\n{net}"
	);
}

#[test]
fn volume_unit_emits_options_and_custom_name() {
	let yaml = r#"
services:
  s:
    image: x
volumes:
  vol:
    name: custom-vol
    driver: local
    driver_opts:
      type: nfs
      device: ":/exports"
      o: "addr=10.0.0.1,rw"
      custom: extra
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let vol = &unit_named(&out, "p-vol.volume").contents;
	assert!(vol.contains("VolumeName=custom-vol"), "in:\n{vol}");
	assert!(!vol.contains("VolumeName=p_vol"), "in:\n{vol}");
	// `local` driver opts map to dedicated keys; `o` is a single mount-option
	// string. Options= without a Device= would be rejected by Quadlet.
	assert!(vol.contains("Type=nfs"), "in:\n{vol}");
	assert!(vol.contains("Device=:/exports"), "in:\n{vol}");
	assert!(vol.contains("Options=addr=10.0.0.1,rw"), "in:\n{vol}");
	// An option with no dedicated key falls back to PodmanArgs=--opt. The
	// value is quoted (#1734): systemd consumes the quotes, so Podman's own
	// generator still builds `--opt custom=extra`, and a value carrying
	// whitespace stays one argv element instead of becoming extra flags.
	// The assertion is on the argv rather than the unit text for that
	// reason: the unit text is what looked correct while the bug was live.
	assert_eq!(
		crate::quadlet::tests::podman_argv::podman_argv_from_unit(vol),
		vec!["--opt", "custom=extra"],
		"in:\n{vol}"
	);
}

#[test]
fn healthcheck_start_interval_is_warned_and_omitted() {
	// `start_interval` has no Quadlet/Podman equivalent: it must not emit a
	// `HealthStartupInterval=` (which drives an unrelated, no-op startup
	// healthcheck) and must instead produce a warning.
	let yaml = r#"
services:
  s:
    image: x
    healthcheck:
      test: ["CMD", "true"]
      interval: 5s
      start_interval: 2s
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("HealthInterval=5s"), "in:\n{c}");
	assert!(
		!c.contains("HealthStartupInterval"),
		"start_interval must not emit HealthStartupInterval=; got:\n{c}"
	);
	let joined = out.warnings.join("\n");
	assert!(
		joined.contains("start_interval"),
		"start_interval must warn; got:\n{joined}"
	);
}

#[test]
fn dependency_unit_names_are_sanitized_in_ordering() {
	// A dependency whose compose key sanitizes to a different stem must be
	// referenced by that stem in After=/Requires=, matching the generated unit.
	let yaml = r#"
services:
  web:
    image: nginx
    depends_on:
      - "db:1"
  ? "db:1"
  : { image: postgres }
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
	let web = &unit_named(&out, "proj-web.container").contents;
	assert!(web.contains("After=proj-db_1.service"), "in:\n{web}");
	assert!(web.contains("Requires=proj-db_1.service"), "in:\n{web}");
	// The raw, unsanitized name must not leak into the ordering directives.
	assert!(!web.contains("db:1.service"), "in:\n{web}");
	// The dependency's own unit really is named with the sanitized stem.
	unit_named(&out, "proj-db_1.container");
}

#[test]
fn network_mode_service_maps_to_dot_container() {
	// `network_mode: service:X` reuses a sibling service's netns, which Quadlet
	// expresses as the `Network={X}.container` unit dependency. Generated unit
	// stems are project-prefixed, so the dependency is `p-db.container`.
	let yaml = "services:\n  s:\n    image: x\n    network_mode: \"service:db\"\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("Network=p-db.container"),
		"service:db must map to Network=p-db.container; got:\n{c}"
	);
}

#[test]
fn network_mode_container_maps_to_join_form() {
	// `network_mode: container:X` joins an *existing* container's netns by id/name
	// via podman's `Network=container:X`; it is not a `.container` unit dependency
	// (that would name a non-existent unit and fail to start).
	let yaml = "services:\n  s:\n    image: x\n    network_mode: \"container:sidecar\"\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("Network=container:sidecar"),
		"container:sidecar must map to the join form; got:\n{c}"
	);
	assert!(
		!c.contains("sidecar.container"),
		"container: must not emit a .container unit dependency; got:\n{c}"
	);
}

#[test]
fn duplicate_network_aliases_are_emitted_once() {
	// A repeated alias must not produce duplicate `NetworkAlias=` lines, which
	// podman may reject at container create.
	let yaml = "services:\n  s:\n    image: x\n    networks:\n      front:\n        aliases: [dup, dup, uniq]\nnetworks:\n  front:\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert_eq!(
		c.matches("NetworkAlias=dup").count(),
		1,
		"duplicate alias must be emitted once; got:\n{c}"
	);
	assert_eq!(c.matches("NetworkAlias=uniq").count(), 1, "in:\n{c}");
}

#[test]
fn ipam_options_warn_because_quadlet_cannot_emit_them() {
	// The live engine forwards `ipam.options` via the libpod API, but podman
	// network create / Quadlet expose no key for them, so `generate` warns instead
	// of silently diverging.
	let yaml = "networks:\n  net:\n    ipam:\n      options:\n        foo: bar\nservices:\n  s:\n    image: x\n    networks: [net]\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	assert!(
		out.warnings.iter().any(|w| w.contains("ipam.options")),
		"expected an ipam.options warning; got: {:?}",
		out.warnings
	);
}

#[test]
fn network_mode_service_target_is_sanitized() {
	let yaml = "services:\n  s:\n    image: x\n    network_mode: \"service:web:1\"\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("Network=p-web_1.container"), "in:\n{c}");
}

#[test]
fn volume_and_network_units_have_no_install_section() {
	let yaml = r#"
services:
  s:
    image: x
networks:
  net:
volumes:
  vol:
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let net = &unit_named(&out, "p-net.network").contents;
	let vol = &unit_named(&out, "p-vol.volume").contents;
	assert!(
		!net.contains("[Install]") && !net.contains("WantedBy"),
		".network must carry no [Install]; got:\n{net}"
	);
	assert!(
		!vol.contains("[Install]") && !vol.contains("WantedBy"),
		".volume must carry no [Install]; got:\n{vol}"
	);
	// The container unit still carries its [Install].
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("[Install]") && c.contains("WantedBy=default.target"));
}
