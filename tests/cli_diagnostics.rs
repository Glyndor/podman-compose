//! CLI-level checks for diagnostic surfacing: forward-compat warnings must be
//! visible with no `RUST_LOG` set, must go to stderr, and must carry the
//! `podup:` program prefix, while stdout stays a clean YAML pipe for `config`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

/// Write `contents` as a compose file in a temp dir of its own.
///
/// The directory must be unique **per call**, not per process: cargo runs the
/// tests in one binary as threads, so a path keyed on the pid is shared by every
/// test in this file. Two tests writing the same compose file then race:
/// `fs::write` truncates before it writes, so a concurrent reader can see an
/// empty file and fail validation. Returning the [`TempDir`] hands the caller
/// the directory's lifetime: hold it for the length of the test, and it is
/// removed on drop rather than left in /tmp forever.
fn compose_file(name: &str, contents: &str) -> (TempDir, PathBuf) {
	let dir = tempfile::tempdir().expect("create temp dir");
	let path = dir.path().join(name);
	fs::write(&path, contents).expect("write compose file");
	(dir, path)
}

/// A compose file with an unsupported key, which drives the forward-compat
/// diagnostic.
fn compose_with_unknown_key() -> (TempDir, PathBuf) {
	compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: nginx:1.27\n    bogus_field: oops\n",
	)
}

#[test]
fn config_warns_on_stderr_with_clean_stdout_and_no_rust_log() {
	let (_dir, file) = compose_with_unknown_key();
	let output = Command::new(bin())
		.arg("-f")
		.arg(&file)
		.arg("config")
		.env_remove("RUST_LOG")
		.output()
		.expect("run podup config");

	assert!(output.status.success(), "config exits cleanly");

	let stdout = String::from_utf8_lossy(&output.stdout);
	let stderr = String::from_utf8_lossy(&output.stderr);

	// The diagnostic fires by default (no RUST_LOG) and lands on stderr with
	// the unified `podup: warning:` prefix.
	assert!(
		stderr.contains("podup: warning:"),
		"warning prefixed and on stderr; got stderr:\n{stderr}"
	);
	assert!(
		stderr.contains("bogus_field"),
		"warning names the unknown key; got stderr:\n{stderr}"
	);

	// stdout is the resolved YAML only, with no diagnostics interleaved.
	assert!(stdout.contains("services:"), "stdout carries the YAML");
	assert!(
		!stdout.contains("warning"),
		"stdout stays a clean pipe; got stdout:\n{stdout}"
	);
}

#[test]
fn completions_emit_a_script_to_stdout() {
	let output = Command::new(bin())
		.args(["completions", "bash"])
		.output()
		.expect("run podup completions bash");
	assert!(output.status.success(), "completions exits cleanly");
	let stdout = String::from_utf8_lossy(&output.stdout);
	assert!(
		stdout.contains("_podup") || stdout.contains("complete"),
		"emits a bash completion script; got:\n{}",
		&stdout[..stdout.len().min(200)]
	);
}

/// A minimal valid two-service compose file in a fresh temp dir.
fn compose_two_services() -> (TempDir, PathBuf) {
	compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: nginx:1.27\n  db:\n    image: postgres:16\n",
	)
}

#[test]
fn config_services_lists_service_names() {
	let (_dir, file) = compose_two_services();
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "config", "--services"])
		.output()
		.expect("run config --services");
	assert!(out.status.success());
	let names: Vec<&str> = std::str::from_utf8(&out.stdout).unwrap().lines().collect();
	assert!(
		names.contains(&"web") && names.contains(&"db"),
		"got: {names:?}"
	);
}

#[test]
fn config_format_json_emits_json() {
	let (_dir, file) = compose_two_services();
	let out = Command::new(bin())
		.args(["-f", file.to_str().unwrap(), "config", "--format", "json"])
		.output()
		.expect("run config --format json");
	assert!(out.status.success());
	let stdout = String::from_utf8_lossy(&out.stdout);
	let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
	assert!(parsed["services"]["web"].is_object());
}

#[test]
fn config_quiet_validates_without_output() {
	let (_dir, valid) = compose_two_services();
	let ok = Command::new(bin())
		.args(["-f", valid.to_str().unwrap(), "config", "-q"])
		.output()
		.unwrap();
	assert!(ok.status.success());
	assert!(ok.stdout.is_empty(), "quiet must print nothing");

	// A syntactically broken compose file fails validation (non-zero exit).
	let (_bad_dir, badfile) = compose_file(
		"compose.yaml",
		"services:\n  web:\n    image: [unterminated\n",
	);
	let err = Command::new(bin())
		.args(["-f", badfile.to_str().unwrap(), "config", "-q"])
		.output()
		.unwrap();
	assert!(!err.status.success(), "invalid config must fail under -q");
}

#[test]
fn config_no_interpolate_keeps_placeholders_literal() {
	let (_dir, compose) = compose_file(
		"docker-compose.yml",
		"services:\n  web:\n    image: \"alpine:${PODUP_TAG}\"\n",
	);
	let c = compose.to_str().unwrap();

	// Default config interpolates ${PODUP_TAG} (unset → empty).
	let interp = Command::new(bin())
		.args(["-f", c, "config"])
		.env_remove("PODUP_TAG")
		.output()
		.unwrap();
	assert!(interp.status.success());
	assert!(
		!String::from_utf8_lossy(&interp.stdout).contains("${PODUP_TAG}"),
		"default config must interpolate the placeholder"
	);

	// --no-interpolate leaves it literal.
	let raw = Command::new(bin())
		.args(["-f", c, "config", "--no-interpolate"])
		.env_remove("PODUP_TAG")
		.output()
		.unwrap();
	assert!(
		raw.status.success(),
		"config --no-interpolate failed: {:?}",
		raw.stderr
	);
	assert!(
		String::from_utf8_lossy(&raw.stdout).contains("${PODUP_TAG}"),
		"--no-interpolate must keep ${{PODUP_TAG}} literal"
	);
}

/// `--no-recreate` and `--force-recreate` are mutually exclusive: clap must
/// reject them together at parse time (exit non-zero) rather than silently
/// letting force-recreate win, matching docker compose.
#[test]
fn up_rejects_no_recreate_with_force_recreate() {
	let output = Command::new(bin())
		.args(["up", "--no-recreate", "--force-recreate"])
		.output()
		.expect("run up with conflicting flags");
	assert!(
		!output.status.success(),
		"conflicting recreate flags must be rejected"
	);
	let stderr = String::from_utf8_lossy(&output.stderr);
	assert!(
		stderr.contains("cannot be used with") || stderr.contains("conflict"),
		"expected a clap conflict error; got:\n{stderr}"
	);
}

/// The same exclusivity holds for `create`.
#[test]
fn create_rejects_no_recreate_with_force_recreate() {
	let output = Command::new(bin())
		.args(["create", "--no-recreate", "--force-recreate"])
		.output()
		.expect("run create with conflicting flags");
	assert!(
		!output.status.success(),
		"conflicting recreate flags must be rejected"
	);
	// Which rejection, not just that one happened. A non-zero exit here is
	// satisfied by any failure (a missing compose file, an unreachable Podman,
	// a panic) none of which exercise the flag conflict this test is named for.
	let err = String::from_utf8_lossy(&output.stderr);
	assert!(
		err.contains("--no-recreate") && err.contains("--force-recreate"),
		"the rejection did not name the two conflicting flags: {err:?}"
	);
}

#[test]
fn config_warns_on_unset_interpolation_variable() {
	// An unset `${VAR}` interpolates to an empty string but must warn on stderr
	// (matching docker compose) so a config typo does not pass silently.
	let (_dir, compose) = compose_file(
		"docker-compose.yml",
		"services:\n  web:\n    image: ${PODUP_UNSET_IMAGE}\n",
	);
	let c = compose.to_str().unwrap();

	let out = Command::new(bin())
		.args(["-f", c, "config"])
		.env_remove("PODUP_UNSET_IMAGE")
		.output()
		.unwrap();
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("PODUP_UNSET_IMAGE") && stderr.contains("not set"),
		"expected an unset-variable warning on stderr, got: {stderr:?}"
	);
}

#[test]
fn config_no_interpolate_skips_required_var_error() {
	// `--no-interpolate` must not evaluate a required-var `${VAR:?msg}`: with the
	// variable unset the command should still succeed and print the placeholder
	// literally, rather than failing on the required-var check.
	let (_dir, compose) = compose_file(
		"docker-compose.yml",
		"services:\n  web:\n    image: ${MUST_SET:?required}\n",
	);
	let c = compose.to_str().unwrap();

	// With interpolation on, the required-var error fails the command.
	let interp = Command::new(bin())
		.args(["-f", c, "config"])
		.env_remove("MUST_SET")
		.output()
		.unwrap();
	assert!(
		!interp.status.success(),
		"default config must fail on the required-var"
	);

	// With --no-interpolate, the file is printed uninterpolated and succeeds.
	let raw = Command::new(bin())
		.args(["-f", c, "config", "--no-interpolate"])
		.env_remove("MUST_SET")
		.output()
		.unwrap();
	assert!(
		raw.status.success(),
		"config --no-interpolate must not evaluate the required-var: {:?}",
		String::from_utf8_lossy(&raw.stderr)
	);
	assert!(
		String::from_utf8_lossy(&raw.stdout).contains("${MUST_SET:?required}"),
		"--no-interpolate must keep the placeholder literal"
	);
}

/// `update` only rewrites the podup binary, so the compose-only global value
/// flags (--socket/--profile/--env-file/--project-directory) cannot affect it.
/// Passing one on the command line is rejected as a usage error rather than
/// silently accepted as a no-op, and the rejection happens before any network
/// access, so the test never reaches GitHub.
///
/// Gated on the `update` feature: package-manager builds (Debian/apt) compile
/// without it, so the `update` subcommand does not exist there.
#[cfg(feature = "update")]
#[test]
fn update_rejects_compose_only_global_flags() {
	for flag in [
		"--socket=unix:///tmp/nope.sock",
		"--profile=dev",
		"--env-file=/tmp/nope.env",
		"--project-directory=/tmp",
	] {
		let output = Command::new(bin())
			.args([flag, "update", "--check"])
			.env_remove("PODMAN_SOCKET")
			.env_remove("COMPOSE_PROFILES")
			.output()
			.expect("run podup update with a compose-only flag");
		assert!(
			!output.status.success(),
			"`{flag}` must be rejected for update, got success"
		);
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert!(
			stderr.contains("no effect on `update`"),
			"rejection should explain the misuse for {flag}; got stderr:\n{stderr}"
		);
	}
}

/// The `-f -` form enforces the same 16 MiB read cap the file path does.
///
/// The cap exists so a pathological compose document cannot exhaust memory, and
/// the generic reader that implements it is unit-tested, but with an explicit
/// limit passed in. **Nothing checked that the stdin call site passes
/// `MAX_FILE_BYTES` rather than something larger**, which a mutation replacing it
/// with `u64::MAX` proved by surviving the whole suite.
///
/// It cannot be closed at unit level: the function reads the real stdin, so the
/// only honest test is the one a user would perform. This feeds the binary more
/// than the cap through a pipe and asserts it is refused.
#[test]
fn stdin_is_refused_past_the_read_cap() {
	use std::io::Write;
	use std::process::Stdio;

	// One byte over 16 MiB, and valid YAML up to the point it is rejected, so a
	// refusal cannot be mistaken for a parse error. The padding lives in a
	// comment for the same reason.
	const CAP: usize = 16 * 1024 * 1024;
	let mut document = String::from("services:\n  web:\n    image: alpine\n# ");
	document.push_str(&"x".repeat(CAP + 1 - document.len()));
	document.push('\n');
	assert!(document.len() > CAP, "the fixture must exceed the cap");

	let mut child = Command::new(bin())
		.args(["-f", "-", "config"])
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	// A refused read closes the pipe, so the write can fail with EPIPE before
	// the whole document is sent. That is the success path, not an error.
	let _ = child.stdin.take().unwrap().write_all(document.as_bytes());
	let out = child.wait_with_output().unwrap();

	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		!out.status.success(),
		"oversized stdin was accepted: stdout {} bytes, stderr {stderr:?}",
		out.stdout.len()
	);
	assert!(
		stderr.contains("larger than") && stderr.contains("limit"),
		"refused for some other reason than the cap: {stderr:?}"
	);
}

/// The same document one byte under the cap is accepted, so the test above
/// pins a threshold rather than "big inputs fail".
#[test]
fn stdin_just_under_the_cap_is_accepted() {
	use std::io::Write;
	use std::process::Stdio;

	const CAP: usize = 16 * 1024 * 1024;
	let mut document = String::from("services:\n  web:\n    image: alpine\n# ");
	document.push_str(&"x".repeat(CAP - document.len() - 1));
	document.push('\n');
	assert!(document.len() <= CAP, "the fixture must fit under the cap");

	let mut child = Command::new(bin())
		.args(["-f", "-", "config"])
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	child
		.stdin
		.take()
		.unwrap()
		.write_all(document.as_bytes())
		.unwrap();
	let out = child.wait_with_output().unwrap();
	assert!(
		out.status.success(),
		"a document under the cap was refused: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}

/// A compose file past the 16 MiB cap is refused, the same as stdin.
///
/// The cap is a bound on what a single trusted-but-unbounded input can make
/// podup allocate. `read_capped_from` implements it and is unit-tested, with an
/// explicit limit passed in, so **what was untested is that the real call sites
/// pass `MAX_FILE_BYTES`**. A mutation raising the constant to a terabyte left
/// the whole lib and bins suite green, which is what a control nothing exercises
/// looks like from outside.
#[test]
fn a_compose_file_past_the_read_cap_is_refused() {
	const CAP: usize = 16 * 1024 * 1024;
	let dir = TempDir::new().unwrap();
	let path = dir.path().join("docker-compose.yml");
	let mut document = String::from("services:\n  web:\n    image: alpine\n# ");
	document.push_str(&"x".repeat(CAP + 1 - document.len()));
	document.push('\n');
	fs::write(&path, &document).unwrap();

	let out = Command::new(bin())
		.args(["-f", path.to_str().unwrap(), "config"])
		.output()
		.unwrap();
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		!out.status.success(),
		"an oversized compose file was accepted: {} bytes of stdout",
		out.stdout.len()
	);
	assert!(
		stderr.contains("larger than") && stderr.contains("limit"),
		"refused for some other reason than the cap: {stderr:?}"
	);
}

/// The same file one byte under the cap is accepted, so the test above pins the
/// threshold rather than "large files fail".
#[test]
fn a_compose_file_just_under_the_read_cap_is_accepted() {
	const CAP: usize = 16 * 1024 * 1024;
	let dir = TempDir::new().unwrap();
	let path = dir.path().join("docker-compose.yml");
	let mut document = String::from("services:\n  web:\n    image: alpine\n# ");
	document.push_str(&"x".repeat(CAP - document.len() - 1));
	document.push('\n');
	fs::write(&path, &document).unwrap();

	let out = Command::new(bin())
		.args(["-f", path.to_str().unwrap(), "config"])
		.output()
		.unwrap();
	assert!(
		out.status.success(),
		"a compose file under the cap was refused: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}
