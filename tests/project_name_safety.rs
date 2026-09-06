//! Boundary guard: the CLI must reject an unsafe project name before it reaches
//! any code path that builds a filesystem path from it.
//!
//! `--project` / `COMPOSE_PROJECT_NAME` and the compose `name:` field are taken
//! verbatim, so a traversal value like `../evil` must be rejected up front,
//! including on the `generate quadlet` path, which neither locks nor stages and
//! would otherwise never consult `is_safe_project_name`.

use std::fs;
use std::process::Command;

/// Write a minimal valid compose file into a fresh temp dir and return the dir.
fn compose_dir() -> tempfile::TempDir {
	let dir = tempfile::tempdir().expect("tempdir");
	fs::write(
		dir.path().join("docker-compose.yml"),
		"services:\n  web:\n    image: alpine\n",
	)
	.expect("write compose");
	dir
}

fn podup() -> Command {
	Command::new(env!("CARGO_BIN_EXE_podup"))
}

#[test]
fn rejects_traversal_project_name() {
	let dir = compose_dir();
	let out = podup()
		.current_dir(dir.path())
		.args(["-p", "../evil", "generate", "quadlet"])
		.output()
		.expect("run podup");

	assert!(
		!out.status.success(),
		"unsafe project name must fail; stdout: {}",
		String::from_utf8_lossy(&out.stdout)
	);
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("not a safe path component"),
		"error must name the boundary check; got: {stderr}"
	);
}

#[test]
fn rejects_project_name_from_env() {
	let dir = compose_dir();
	let out = podup()
		.current_dir(dir.path())
		.env("COMPOSE_PROJECT_NAME", "../../etc")
		.args(["generate", "quadlet"])
		.output()
		.expect("run podup");

	assert!(
		!out.status.success(),
		"unsafe COMPOSE_PROJECT_NAME must fail"
	);
	assert!(String::from_utf8_lossy(&out.stderr).contains("not a safe path component"));
}

#[test]
fn rejects_leading_dash_project_name() {
	// A leading dash is a latent flag-injection vector and is rejected by
	// docker-compose's `^[a-z0-9][a-z0-9_-]*$` rule. `-p=` keeps clap from
	// treating the value itself as a flag so the boundary check is what fails.
	let dir = compose_dir();
	let out = podup()
		.current_dir(dir.path())
		.args(["-p=-rf", "generate", "quadlet"])
		.output()
		.expect("run podup");

	assert!(!out.status.success(), "leading-dash project name must fail");
	assert!(String::from_utf8_lossy(&out.stderr).contains("not a safe path component"));
}

#[test]
fn rejects_uppercase_project_name() {
	let dir = compose_dir();
	let out = podup()
		.current_dir(dir.path())
		.args(["-p", "MyApp", "generate", "quadlet"])
		.output()
		.expect("run podup");

	assert!(!out.status.success(), "uppercase project name must fail");
	assert!(String::from_utf8_lossy(&out.stderr).contains("not a safe path component"));
}

#[test]
fn accepts_safe_project_name() {
	let dir = compose_dir();
	let out = podup()
		.current_dir(dir.path())
		.args(["-p", "my-app", "generate", "quadlet"])
		.output()
		.expect("run podup");

	assert!(
		out.status.success(),
		"a safe project name must be accepted; stderr: {}",
		String::from_utf8_lossy(&out.stderr)
	);
}
