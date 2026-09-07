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

use super::assert_argv_has_no_token;
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
		c.contains("PodmanArgs=--memory=\"512m\""),
		"mem_limit must route through PodmanArgs in:\n{c}"
	);
	assert!(
		c.contains("PodmanArgs=--security-opt apparmor=\"my-profile\""),
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
		"PodmanArgs=--cpus=\"1.5\"",
		"PodmanArgs=--cpuset-cpus=\"0,1\"",
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
		c.contains("PodmanArgs=--cpus=\"2\""),
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

// ---------------------------------------------------------------------------
// #1734: PodmanArgs= argv safety at the seven interpolation sites
//
// `escape_unit_value` returns early for `PodmanArgs` (treating it like
// `Exec`/`Entrypoint`), so the seven `format!()` sites that interpolate a
// compose value into a podman-flag template were emitting raw bytes. A
// hostile compose value containing whitespace would smuggle additional
// `podman run` flags onto the same `PodmanArgs=` line; the unit text reads as
// one innocent line, but systemd's word-splitter and podman's parser see two
// argv elements. The only assertion worth pinning is the argv podman would
// actually build, not the line that produced it.
// ---------------------------------------------------------------------------

#[test]
fn mem_limit_cannot_smuggle_extra_flags() {
	// `mem_limit: "512m --privileged -v /:/hostfs"` is the canonical case
	// from the issue. The smuggled `--privileged` and `-v /:/hostfs` must
	// land inside ONE argv element via quoting, not become separate podman
	// arguments.
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    mem_limit: "512m --privileged -v /:/hostfs"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-web.container").contents;
	assert_argv_has_no_token(c, "--privileged");
	assert_argv_has_no_token(c, "-v");
}

#[test]
fn cpus_cannot_smuggle_extra_flags() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    cpus: "1.5 --privileged"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-web.container").contents;
	assert_argv_has_no_token(c, "--privileged");
}

#[test]
fn cpuset_cannot_smuggle_extra_flags() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    cpuset: "0 --privileged -v /:/hostfs2"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-web.container").contents;
	assert_argv_has_no_token(c, "--privileged");
	assert_argv_has_no_token(c, "-v");
}

#[test]
fn deploy_memory_cannot_smuggle_extra_flags() {
	// `deploy.resources.limits.memory` is the modern equivalent of
	// `mem_limit`; both interpolation sites need the same protection.
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    deploy:
      resources:
        limits:
          memory: "256m --privileged"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-web.container").contents;
	assert_argv_has_no_token(c, "--privileged");
}

#[test]
fn apparmor_profile_cannot_smuggle_extra_flags() {
	// The apparmor arm routes through PodmanArgs too (`AppArmor=` would be
	// dropped by Quadlet). A hostile profile like
	// "my-profile --privileged -v /:/hostfs" must stay one argv element.
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    security_opt:
      - "apparmor=my-profile --privileged"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-web.container").contents;
	assert_argv_has_no_token(c, "--privileged");
}

#[test]
fn build_arg_value_cannot_smuggle_extra_flags() {
	// The smoke-test from the issue: a build service with a `build:` block
	// emits a `.build` unit that carries `--build-arg` through PodmanArgs.
	// A value with whitespace must not become two podman args.
	let yaml = r#"
services:
  app:
    build:
      context: .
      args:
        EVIL: "v --privileged -v /:/hostfs"
    image: app:1.0
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let b = &unit_named(&out, "p-app.build").contents;
	assert_argv_has_no_token(b, "--privileged");
	assert_argv_has_no_token(b, "-v");
}

#[test]
fn build_arg_bare_key_cannot_smuggle_extra_flags() {
	// A `--build-arg` with no value is just the key. Smuggling still has to
	// be impossible: a hostile key like `BAD --privileged` must end up as
	// one argv element, not the literal "--privileged" plus the key.
	let yaml = r#"
services:
  app:
    build:
      context: .
      args:
        - "BAD --privileged"
    image: app:1.0
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let b = &unit_named(&out, "p-app.build").contents;
	assert_argv_has_no_token(b, "--privileged");
}

#[test]
fn podman_args_doubles_percent_specifiers() {
	// systemd specifiers like `%h` would otherwise be expanded at unit
	// activation time (the same behaviour `Environment=` already escapes).
	// After the fix a value carrying `%h` is doubled to `%%h` in the
	// interpolated podman arg, so podman receives `%h` literally and systemd
	// does not substitute it for the unit's hostname.
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    mem_limit: "%h/mem"
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-web.container").contents;
	// After systemd unescapes `%%` to `%`, podman would receive the
	// literal `%h/mem` from the quoted value, which is harmless; before
	// the fix, podman would receive `mem` with `%h` expanded to the host's
	// hostname.
	assert!(
		c.contains("PodmanArgs=--memory=\"%%h/mem\""),
		"`%%h` must be doubled on the PodmanArgs path; got:\n{c}"
	);
}
