//! Resource-limit and `deploy:` rendering tests for the Quadlet export.
//!
//! `internal/quadlet/tests/fields.rs` carries the rest of the field-mapping
//! coverage. These are pulled out for the same reason `fields_logging.rs` was
//! (#1354): the file was 481 code lines against a 500-line hard limit, which
//! means the pull request that broke it would have been whichever one next
//! added a test here, not the one that made the file long.
//!
//! The cut is by subject rather than by size. Everything here renders a limit
//! or a `deploy:` key into a Podman argument or a systemd directive, which is
//! one question; the rest of `fields.rs` maps compose fields to Quadlet keys.

use super::unit_named;
use crate::parse_str;
use crate::quadlet::generate_at;

#[test]
fn memory_and_apparmor_render_as_podman_args() {
	// `Memory=` and `AppArmor=` are not recognised [Container] keys in
	// podman-systemd.unit(5) (Quadlet drops the whole unit at daemon-reload), so
	// they must route through `PodmanArgs=` like the CPU limits, not be emitted as
	// native keys.
	let yaml = r#"
services:
  s:
    image: app:1.0
    mem_limit: 512m
    security_opt:
      - "apparmor=my-profile"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("PodmanArgs=--memory=512m"),
		"mem_limit must route through PodmanArgs in:\n{c}"
	);
	assert!(
		c.contains("PodmanArgs=--security-opt apparmor=my-profile"),
		"apparmor must route through PodmanArgs in:\n{c}"
	);
	for forbidden in ["Memory=512m", "AppArmor=my-profile"] {
		assert!(
			!c.contains(forbidden),
			"memory/apparmor must not use an unrecognised native key `{forbidden}` in:\n{c}"
		);
	}
}

#[test]
fn cpu_limits_render_as_podman_args() {
	// CPU limits have no native [Container] Quadlet key; they must round-trip
	// through PodmanArgs= rather than being silently dropped.
	let yaml = r#"
services:
  s:
    image: app:1.0
    cpus: "1.5"
    cpuset: "0,1"
    cpu_shares: 512
    cpu_quota: 50000
    cpu_period: 100000
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	for expected in [
		"PodmanArgs=--cpus=1.5",
		"PodmanArgs=--cpuset-cpus=0,1",
		"PodmanArgs=--cpu-shares=512",
		"PodmanArgs=--cpu-quota=50000",
		"PodmanArgs=--cpu-period=100000",
	] {
		assert!(c.contains(expected), "missing `{expected}` in:\n{c}");
	}
}

#[test]
fn deploy_limits_cpus_render_as_podman_args() {
	// `deploy.resources.limits.cpus` is the modern equivalent of `cpus` and
	// must reach the unit too when the top-level `cpus` is absent.
	let yaml = r#"
services:
  s:
    image: app:1.0
    deploy:
      resources:
        limits:
          cpus: "2"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("PodmanArgs=--cpus=2"),
		"missing deploy cpus PodmanArgs in:\n{c}"
	);
}

#[test]
fn deploy_limits_pids_maps_to_pids_limit() {
	let yaml = r#"
services:
  s:
    image: x
    deploy:
      resources:
        limits:
          pids: 256
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("PidsLimit=256"), "missing PidsLimit in:\n{c}");
}

#[test]
fn deploy_restart_policy_maps_to_systemd() {
	let yaml = r#"
services:
  s:
    image: x
    deploy:
      restart_policy:
        condition: on-failure
        max_attempts: 4
        window: 2m
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(c.contains("Restart=on-failure"), "in:\n{c}");
	assert!(c.contains("StartLimitBurst=4"), "in:\n{c}");
	assert!(c.contains("StartLimitIntervalSec=120"), "in:\n{c}");
}

#[test]
fn deploy_restart_condition_none_maps_to_no() {
	let yaml = "services:\n  s:\n    image: x\n    deploy:\n      restart_policy:\n        condition: none\n";
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	assert!(unit_named(&out, "p-s.container")
		.contents
		.contains("Restart=no"));
}
