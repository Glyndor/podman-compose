//! Integration tests for `create` (containers without starting) and `ls`
//! (project discovery) against a real Podman daemon. Skip when unreachable.
use std::fs;
use tempfile::tempdir;

use super::*;

/// Count non-empty lines of a `-q` listing.
fn count(out: &std::process::Output) -> usize {
	String::from_utf8_lossy(&out.stdout)
		.lines()
		.filter(|l| !l.trim().is_empty())
		.count()
}

#[tokio::test]
async fn create_makes_containers_without_starting_them() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-create", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let create = run(&["-f", c, "-p", &proj, "create"]);
	assert!(
		create.status.success(),
		"create failed: {:?}",
		create.stderr
	);
	// The container exists (visible with -a) but is not running.
	assert_eq!(count(&run(&["-f", c, "-p", &proj, "ps", "-a", "-q"])), 1);
	assert_eq!(
		count(&run(&["-f", c, "-p", &proj, "ps", "-q"])),
		0,
		"create must not start the container"
	);

	// `up` then starts the already-created container.
	run_ok(&["-f", c, "-p", &proj, "up", "-d"]);
	assert_eq!(count(&run(&["-f", c, "-p", &proj, "ps", "-q"])), 1);

	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn ls_lists_running_projects() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-lsproj", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run_ok(&["-f", c, "-p", &proj, "up", "-d"]);
	// `ls -q` (running only) lists the project by name.
	let names = String::from_utf8_lossy(&run(&["-p", &proj, "ls", "-q"]).stdout).into_owned();
	assert!(
		names.lines().any(|l| l == proj),
		"ls must list {proj}: {names}"
	);

	run(&["-f", c, "-p", &proj, "down"]);
	// After teardown the project has no containers, so it drops from `ls`.
	let after = String::from_utf8_lossy(&run(&["-p", &proj, "ls", "-q"]).stdout).into_owned();
	assert!(
		!after.lines().any(|l| l == proj),
		"a torn-down project must not appear in ls: {after}"
	);
}
