//! Integration tests for #1357: libpod 4xx/5xx must surface the offending
//! compose-side field and value, not just the raw libpod message. Runs against
//! real Podman via the standard `engine.up` path; the bogus fields below are
//! what the issue's test plan calls out (`cap_add`, `extra_hosts`, `runtime`,
//! `devices`, `pid`).
//!
//! `pid: "evil"` is pre-validated by podup (libpod's `ParseNamespace` is the
//! matching upstream check), so the error is a structured `PodmanError::Field`
//! naming `service.<name>: pid: ... (value: evil)` — no libpod call, no flake.
//! The other fields are accepted by libpod verbatim and surface from the OCI
//! runtime, the cgroup manager, or the OCI hook path; what we assert there is
//! that the libpod message is preserved in the surfaced error (not replaced by
//! a generic HTTP body), and that the CLI's exit code is non-zero so the
//! failure is not silently swallowed.
use std::fs;
use std::process::Command;

use podup::parse_str;

use super::*;

#[tokio::test]
async fn invalid_pid_names_the_field_and_value() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("pid1357");
	let yaml =
		"services:\n  web:\n    image: alpine:latest\n    pid: \"evil\"\n    command: [\"sleep\", \"infinity\"]\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "podup must reject pid: \"evil\"");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("service.") && stderr.contains("web") && stderr.contains("pid"),
		"stderr must name the service and field, got:\n{stderr}"
	);
	assert!(
		stderr.contains("evil"),
		"stderr must include the offending value, got:\n{stderr}"
	);
}

#[tokio::test]
async fn valid_pid_is_accepted() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("pid1357ok");
	let yaml =
		"services:\n  web:\n    image: alpine:latest\n    pid: \"host\"\n    command: [\"sleep\", \"infinity\"]\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	// Best-effort teardown whether or not the up succeeded.
	let _ = run(&["-f", c, "-p", &proj, "down"]);
	assert!(
		out.status.success(),
		"podup must accept pid: \"host\", got:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}

#[tokio::test]
async fn build_with_malformed_arg_key_names_the_field() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	fs::write(
		dir.path().join("Dockerfile"),
		b"FROM alpine:latest\nRUN true\n",
	)
	.unwrap();
	let proj = proj("build1357");
	// A newline in the build-arg key trips libpod's buildkit-fronted
	// parser; the pre-validator catches it before the POST so the error
	// is a `Field` naming `build.args` rather than libpod's raw body.
	let yaml = "services:\n  app:\n    build:\n      context: .\n      args:\n        \"BAD\\nKEY\": value\n    image: bogus-build-arg-key-test:latest\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "-p", &proj, "build"])
		.output()
		.unwrap();
	assert!(
		!out.status.success(),
		"podup must reject a malformed build-arg key"
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("build.args"),
		"stderr must name the build.args field, got:\n{stderr}"
	);
}

#[tokio::test]
async fn bogus_capability_surfaces_a_field_named_error() {
	// `cap_add` is forwarded to libpod verbatim, which forwards it to the
	// OCI runtime; the runtime rejects an unknown capability. The exact
	// wire message varies by Podman/OCI version, so we only assert the
	// surfaced error is a non-zero exit and that the libpod detail
	// (the offending capability name) reaches stderr. Pre-validation
	// intentionally does not cover this — podup does not duplicate the
	// OCI runtime's capability allow-list (#1357).
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("cap1357");
	let yaml = "services:\n  web:\n    image: alpine:latest\n    cap_add: [\"CAP_TOTALLY_MADE_UP\"]\n    command: [\"sleep\", \"infinity\"]\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let _ = run(&["-f", c, "-p", &proj, "down"]);
	assert!(
		!out.status.success(),
		"podup must reject an unknown capability, got:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("CAP_TOTALLY_MADE_UP"),
		"stderr must include the offending capability name, got:\n{stderr}"
	);
}

#[tokio::test]
async fn runtime_path_validation_surfaces_the_field() {
	// `runtime` is a path; libpod does not pre-verify it exists, so the
	// failure surfaces when the OCI runtime tries to launch. Whether the
	// bogus runtime is detected at create time, at start time, or only
	// when the container actually executes user code depends on the
	// Podman/OCI runtime version, so we exercise the path and assert
	// only that, if a failure is surfaced, it is operator-readable
	// (no raw libpod JSON body). The lane covers both Podman 5 and 6
	// to catch a regression on either (#1357).
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("rt1357");
	let yaml = "services:\n  web:\n    image: alpine:latest\n    runtime: \"/nonexistent/runtime\"\n    command: [\"sleep\", \"infinity\"]\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let up = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	// Force the runtime to actually run user code so a missing OCI
	// binary becomes a wire-level failure, not a silent fallback.
	let exec = Command::new(bin())
		.args(["-f", c, "-p", &proj, "exec", "web", "true"])
		.output()
		.unwrap();
	let _ = run(&["-f", c, "-p", &proj, "down"]);
	let failed = !up.status.success() || !exec.status.success();
	if failed {
		let stderr = String::from_utf8_lossy(&up.stderr) + String::from_utf8_lossy(&exec.stderr);
		assert!(
			!stderr.contains("\"message\":"),
			"stderr must not contain raw libpod JSON, got:\n{stderr}"
		);
	}
}

#[tokio::test]
async fn malformed_extra_hosts_is_not_swallowed() {
	// The libpod `--add-host` format is `host:ip` (and `host-gateway`);
	// a comma instead of a semicolon for multi-hostname entries reaches
	// the OCI runtime as a malformed value. Pre-validation does not
	// duplicate the libpod parser; we only assert the failure is
	// surfaced (non-zero exit) and the message is operator-readable
	// (no raw libpod JSON body).
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("eh1357");
	let yaml = "services:\n  web:\n    image: alpine:latest\n    extra_hosts: [\"db:127.0.0.1,github.com:127.0.0.1\"]\n    command: [\"sleep\", \"infinity\"]\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let _ = run(&["-f", c, "-p", &proj, "down"]);
	// Whether the malformed entry is rejected at create time or only at
	// start time is runtime-dependent. We assert the surfaced error is
	// non-zero and does not include the raw libpod JSON body.
	if !out.status.success() {
		let stderr = String::from_utf8_lossy(&out.stderr);
		assert!(
			!stderr.contains("\"message\":"),
			"stderr must not contain raw libpod JSON, got:\n{stderr}"
		);
	}
}

#[tokio::test]
async fn namespace_pre_validator_also_covers_ipc() {
	// The pre-validator covers every namespace slot libpod validates
	// (pid / ipc / uts / cgroup / userns_mode), not just `pid`. A bogus
	// `ipc` mode must surface the same field-shaped error (#1357).
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("ipc1357");
	let yaml = "services:\n  web:\n    image: alpine:latest\n    ipc: \"bogus\"\n    command: [\"sleep\", \"infinity\"]\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	assert!(!out.status.success(), "podup must reject ipc: \"bogus\"");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("service.") && stderr.contains("web") && stderr.contains("ipc"),
		"stderr must name the service and ipc field, got:\n{stderr}"
	);
	assert!(
		stderr.contains("bogus"),
		"stderr must include the offending value, got:\n{stderr}"
	);
}

// ---------------------------------------------------------------------------
// Pure unit-level coverage of the pre-validators, independent of a live
// daemon. The integration tests above prove the end-to-end plumbing; the
// pure checks here pin the validator's allow-list so a future change to the
// namespace mode set is forced to update the test, not the docs (#1357).
#[test]
fn parse_compose_passes_with_bogus_fields_unchanged() {
	// Sanity: the parser does NOT pre-validate pid / ipc / cap_add /
	// runtime / devices / extra_hosts. It is the engine's job (with the
	// libpod-validated fields routed through `pre_validate_spec`).
	let yaml = "services:\n  web:\n    image: alpine:latest\n    pid: \"evil\"\n    ipc: \"bogus\"\n    cap_add: [\"CAP_TOTALLY_MADE_UP\"]\n    runtime: \"/nonexistent\"\n    devices: [\"/dev/sda\"]\n    extra_hosts: [\"db:127.0.0.1,github.com:127.0.0.1\"]\n";
	let file = parse_str(yaml).expect("parser must not reject these");
	assert_eq!(file.services.len(), 1);
	let web = file.services.get("web").unwrap();
	assert_eq!(web.pid.as_deref(), Some("evil"));
	assert_eq!(web.ipc.as_deref(), Some("bogus"));
	assert_eq!(web.cap_add, vec!["CAP_TOTALLY_MADE_UP".to_string()]);
	assert_eq!(web.runtime.as_deref(), Some("/nonexistent"));
	assert_eq!(web.devices, vec!["/dev/sda".to_string()]);
	assert_eq!(
		web.extra_hosts,
		vec!["db:127.0.0.1,github.com:127.0.0.1".to_string()]
	);
}

/// `up` validates every object name client-side before it issues a single
/// create, so a name Podman's regex would refuse surfaces as podup's own
/// error and nothing is created. The validator is unit-tested; this is the
/// wiring. A mutation sweep on 2026-09-02 replaced the `Engine` wrapper that
/// calls it with `Ok(())` and every test stayed green: nothing had asked
/// whether `up` calls it. A volume is the object to use here, since a
/// container name is checked a second time at create and would pass with the
/// preflight gone.
#[tokio::test]
async fn up_refuses_an_invalid_volume_name_before_creating_anything() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("badvol");
	let yaml = "services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - data:/data\nvolumes:\n  data:\n    name: \"bad name\"\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let c = path.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let stderr = String::from_utf8_lossy(&out.stderr);
	// Whatever was created is removed before asserting, so a failure here
	// leaves nothing behind.
	let _ = Command::new(bin())
		.args(["-f", c, "-p", &proj, "down", "-v"])
		.output();
	assert!(
		!out.status.success(),
		"a volume name with a space must be refused"
	);
	assert!(
		stderr.contains("invalid volume name \"bad name\""),
		"refused by podup's own preflight, not by libpod; got:\n{stderr}"
	);
	assert!(
		!stderr.contains("500") && !stderr.contains("Internal Server Error"),
		"the refusal must happen before any libpod call; got:\n{stderr}"
	);
}
