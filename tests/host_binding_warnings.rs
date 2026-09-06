//! CLI integration tests for the host-binding / privilege-escalation
//! warning emitter (#1358).
//!
//! The compose-file paths these tests exercise do not require a Podman
//! daemon: `config` walks the parsed model without contacting one,
//! `generate quadlet` is purely file-system work, and the warning
//! emit happens before any API call on `up`/`create`/`run`. So the
//! suite can run on a host without Podman the same way the parse /
//! `cli_diagnostics` tests do.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

fn compose_file(name: &str, contents: &str) -> (TempDir, PathBuf) {
	let dir = tempfile::tempdir().expect("create temp dir");
	let path = dir.path().join(name);
	fs::write(&path, contents).expect("write compose file");
	(dir, path)
}

/// A compose file that declares every host-binding mode the detector
/// knows about, one per service, so a test can grep stderr for each
/// warning by service name without false positives.
const EVERY_MODE_COMPOSE: &str = "\
services:
  net:
    image: alpine:3.19
    network_mode: host
  priv:
    image: alpine:3.19
    privileged: true
  pid:
    image: alpine:3.19
    pid: host
  ipc:
    image: alpine:3.19
    ipc: host
  uts:
    image: alpine:3.19
    uts: host
  cgroup:
    image: alpine:3.19
    cgroup: host
  userns:
    image: alpine:3.19
    userns_mode: host
  shared_net:
    image: alpine:3.19
    network_mode: \"container:sidecar\"
  shared_pid:
    image: alpine:3.19
    pid: \"container:sidecar\"
  plain:
    image: alpine:3.19
";

#[test]
fn config_surfaces_every_active_host_binding_mode() {
	let (_dir, file) = compose_file("compose.yaml", EVERY_MODE_COMPOSE);
	let out = Command::new(bin())
		.arg("-f")
		.arg(&file)
		.arg("config")
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup config");

	assert!(
		out.status.success(),
		"config must exit cleanly: {:?}",
		out.stderr
	);
	let stderr = String::from_utf8_lossy(&out.stderr);

	// Each declared mode produces its own `podup: warning:` line at default log
	// level: the `config` surface is independent of `--no-warn`, so an
	// operator who never runs `up` still sees the modes in CI logs.
	for (service, field) in [
		("net", "network_mode: host"),
		("priv", "privileged: true"),
		("pid", "pid: host"),
		("ipc", "ipc: host"),
		("uts", "uts: host"),
		("cgroup", "cgroup: host"),
		("userns", "userns_mode: host"),
		("shared_net", "network_mode: container:sidecar"),
		("shared_pid", "pid: container:sidecar"),
	] {
		assert!(
			stderr.contains(&format!("service \"{service}\"")) && stderr.contains(field),
			"config must warn on {service}: {field}; got stderr:\n{stderr}"
		);
	}

	// The plain service must NOT warn; pin that the detector does not
	// over-trigger.
	let plain_warning = stderr
		.lines()
		.find(|l| l.contains("podup: warning:") && l.contains("service \"plain\""));
	assert!(
		plain_warning.is_none(),
		"plain service triggered a warning: {plain_warning:?}"
	);

	// stdout stays a clean YAML pipe for `config`.
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		!stdout.contains("podup: warning:"),
		"diagnostics must stay on stderr; got stdout:\n{stdout}"
	);
}

#[test]
fn config_warning_is_unaffected_by_no_warn() {
	// `config` is the "show me what will happen" command, so `--no-warn`
	// (intended for `up`/`create`/`run`) must not silence it.
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: alpine:3.19\n    network_mode: host\n",
	);
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "--no-warn", "config"])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup --no-warn config");
	assert!(out.status.success(), "{:?}", out.stderr);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("network_mode: host"),
		"--no-warn must not silence config: {stderr}"
	);
}

#[test]
fn config_with_no_active_modes_is_silent() {
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: alpine:3.19\n",
	);
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "config"])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup config");
	assert!(out.status.success(), "{:?}", out.stderr);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		!stderr.contains("shares the host's"),
		"a clean service triggered a host-binding warning: {stderr}"
	);
}

#[test]
fn generate_quadlet_warns_on_host_network_and_privileged() {
	// Quadlet has no Podman client to surface warnings through, so it must
	// emit them at generate time. The mode stays emitted in the unit file
	// (Network=host, PodmanArgs=--privileged); the warning is alongside, not
	// instead of, the mapping.
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  net:\n    image: alpine:3.19\n    network_mode: host\n  priv:\n    image: alpine:3.19\n    privileged: true\n  plain:\n    image: alpine:3.19\n",
	);
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "generate", "quadlet"])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup generate quadlet");
	assert!(out.status.success(), "{:?}", out.stderr);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("service \"net\"") && stderr.contains("network_mode: host"),
		"network_mode: host must warn at generate time; got stderr:\n{stderr}"
	);
	assert!(
		stderr.contains("service \"priv\"") && stderr.contains("privileged: true"),
		"privileged: true must warn at generate time; got stderr:\n{stderr}"
	);
	let plain = stderr
		.lines()
		.find(|l| l.contains("podup: warning:") && l.contains("service \"plain\""));
	assert!(
		plain.is_none(),
		"plain service triggered a warning: {plain:?}"
	);

	// Quadlet output (stdout) still carries the modes.
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("Network=host"),
		"Network=host must appear in the generated unit; got:\n{stdout}"
	);
	assert!(
		stdout.contains("PodmanArgs=--privileged"),
		"PodmanArgs=--privileged must appear in the generated unit; got:\n{stdout}"
	);
}

#[test]
fn generate_quadlet_warns_on_container_namespace_sharing() {
	// `container:<id>` is the same isolation surface as `host` and podman
	// does not warn on it, so the Quadlet path must surface it instead.
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: alpine:3.19\n    network_mode: \"container:sidecar\"\n",
	);
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "generate", "quadlet"])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup generate quadlet");
	assert!(out.status.success(), "{:?}", out.stderr);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("container:sidecar"),
		"network_mode: container:<id> must warn at generate time; got stderr:\n{stderr}"
	);
}

#[test]
fn no_warn_drops_the_host_binding_warnings_at_config_parse() {
	// The `config` command goes through the same detector path. `--no-warn`
	// must still suppress the per-service warning *at config time*? No:
	// `config` ignores `--no-warn` by design (the surface is the whole point
	// of the command). This test pins that decision so a future refactor
	// cannot silently change it: a compose with no host modes + `--no-warn`
	// is silent; a compose *with* modes + `--no-warn` still warns under
	// `config`.
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: alpine:3.19\n    network_mode: host\n",
	);
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "--no-warn", "config"])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup --no-warn config");
	assert!(out.status.success());
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("network_mode: host"),
		"--no-warn must NOT silence config: {stderr}"
	);
}

#[test]
fn unknown_no_warn_after_subcommand_is_accepted() {
	// `--no-warn` is a global flag, so it is equally valid before and after
	// the subcommand. Parsing must succeed (a clap failure would exit
	// non-zero before the warning code ever runs).
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: alpine:3.19\n    network_mode: host\n",
	);
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "config", "--no-warn"])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup config --no-warn");
	assert!(out.status.success(), "{:?}", out.stderr);
}

#[test]
fn generate_quadlet_no_warn_suppresses_the_warnings() {
	// `--no-warn` opts the operator out of the Quadlet-path warnings the
	// same way it does on the live engine. The mode is still emitted in the
	// unit file; only the per-mode `podup: warning:` line is suppressed.
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  net:\n    image: alpine:3.19\n    network_mode: host\n  priv:\n    image: alpine:3.19\n    privileged: true\n",
	);
	let out = Command::new(bin())
		.args([
			"-f",
			file.to_str().unwrap(),
			"--no-warn",
			"generate",
			"quadlet",
		])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup --no-warn generate quadlet");
	assert!(out.status.success(), "{:?}", out.stderr);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		!stderr.contains("podup: warning: network_mode: host"),
		"--no-warn must silence the network_mode warning; got stderr:\n{stderr}"
	);
	assert!(
		!stderr.contains("podup: warning: privileged: true"),
		"--no-warn must silence the privileged warning; got stderr:\n{stderr}"
	);
	// The unit still carries the modes; suppression is the per-line warning,
	// not the emitted directive.
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("Network=host"),
		"Network=host must still be in the unit: {stdout}"
	);
	assert!(
		stdout.contains("PodmanArgs=--privileged"),
		"PodmanArgs=--privileged must still be in the unit: {stdout}"
	);
}

#[test]
fn generate_quadlet_no_warn_without_modes_is_silent() {
	// Sanity: `--no-warn` against a clean compose file produces no
	// host-binding / privilege-escalation warning. (The Quadlet path also
	// emits an unrelated platform advisory on non-Linux hosts, and that is not
	// gated on `--no-warn` and is intentionally left alone, so the assertion
	// filters to the host-binding lines only.)
	let (_dir, file) = compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: alpine:3.19\n",
	);
	let out = Command::new(bin())
		.args([
			"-f",
			file.to_str().unwrap(),
			"--no-warn",
			"generate",
			"quadlet",
		])
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup --no-warn generate quadlet");
	assert!(out.status.success(), "{:?}", out.stderr);
	let stderr = String::from_utf8_lossy(&out.stderr);
	for needle in [
		"network_mode: host",
		"privileged: true",
		"pid: host",
		"ipc: host",
		"uts: host",
		"cgroup: host",
		"userns_mode: host",
		"container:",
	] {
		assert!(
			!stderr.contains(needle),
			"--no-warn must silence the {needle} warning; got stderr:\n{stderr}"
		);
	}
}
