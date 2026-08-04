//! Scale integration tests (`up --scale` and the `scale` subcommand) against a
//! real Podman daemon. Skip gracefully when Podman is unreachable.
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

/// Number of running containers in the project (counts `ps -q` output lines).
fn running_count(compose: &str, proj: &str) -> usize {
	let out = Command::new(bin())
		.args(["-f", compose, "-p", proj, "ps", "-q"])
		.output()
		.unwrap();
	String::from_utf8_lossy(&out.stdout)
		.lines()
		.filter(|l| !l.trim().is_empty())
		.count()
}

/// Podman container id for `name`, or empty when it does not exist.
fn container_id(name: &str) -> String {
	let out = Command::new("podman")
		.args(["inspect", "-f", "{{.Id}}", name])
		.output()
		.unwrap();
	String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn up_scale_creates_replicas_and_down_removes_them() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-upscale", std::process::id());
	fs::write(
		&compose,
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let up = run(&[
		"-f", c, "-p", &proj, "up", "--detach", "--scale", "worker=3",
	]);
	assert!(up.status.success(), "up --scale failed: {:?}", up.stderr);
	assert_eq!(
		running_count(c, &proj),
		3,
		"expected 3 replicas after up --scale"
	);

	let down = run(&["-f", c, "-p", &proj, "down"]);
	assert!(down.status.success(), "down failed: {:?}", down.stderr);
	assert_eq!(
		running_count(c, &proj),
		0,
		"down must remove scaled replicas"
	);
}

#[tokio::test]
async fn scale_subcommand_scales_up_then_down() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-scalecmd", std::process::id());
	fs::write(
		&compose,
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run_ok(&["-f", c, "-p", &proj, "up", "--detach"]);
	assert_eq!(running_count(c, &proj), 1);

	let up = run(&["-f", c, "-p", &proj, "scale", "worker=3"]);
	assert!(up.status.success(), "scale up failed: {:?}", up.stderr);
	assert_eq!(running_count(c, &proj), 3, "scale up must add replicas");

	let down = run(&["-f", c, "-p", &proj, "scale", "worker=1"]);
	assert!(
		down.status.success(),
		"scale down failed: {:?}",
		down.stderr
	);
	assert_eq!(running_count(c, &proj), 1, "scale down must remove surplus");

	run(&["-f", c, "-p", &proj, "down"]);
	assert_eq!(running_count(c, &proj), 0);
}

/// #815 regression: scaling down to one must PRESERVE the surviving replica
/// (`worker-1`), not destroy and recreate it. Containers are always named
/// `worker-1..N`, so the desired set at target 1 is `{worker-1}` — which matches
/// the running `worker-1` and keeps it. Before the fix the desired set was the
/// bare, never-created `worker`, so every numbered replica (incl. the survivor)
/// was removed and a fresh container took its place — losing the container's
/// identity, uptime and in-memory state.
#[tokio::test]
async fn scale_down_to_one_preserves_surviving_replica() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-scalekeep", std::process::id());
	fs::write(
		&compose,
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();
	let survivor = format!("{proj}-worker-1");

	// A single `up` already names the container `worker-1` (always-suffix).
	let up = run(&["-f", c, "-p", &proj, "up", "--detach"]);
	assert!(up.status.success(), "up failed: {:?}", up.stderr);
	let id_before = container_id(&survivor);
	assert!(
		!id_before.is_empty(),
		"single `up` must create the index-suffixed container {survivor}"
	);

	// Scale up to three, then back down to one.
	assert!(run(&["-f", c, "-p", &proj, "scale", "worker=3"])
		.status
		.success());
	assert_eq!(running_count(c, &proj), 3, "scale up must add replicas");
	assert!(run(&["-f", c, "-p", &proj, "scale", "worker=1"])
		.status
		.success());
	assert_eq!(running_count(c, &proj), 1, "scale down must remove surplus");

	// The survivor must be the very same container — same id, never recreated.
	let id_after = container_id(&survivor);
	assert_eq!(
		id_before, id_after,
		"scale-down to 1 must keep {survivor} (id {id_before}), not recreate it (got {id_after})"
	);

	run(&["-f", c, "-p", &proj, "down"]);
	assert_eq!(running_count(c, &proj), 0);
}

#[tokio::test]
async fn up_scale_fixed_host_port_fails_fast() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-scaleport", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    ports:\n      - \"8085:80\"\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let out = run(&["-f", c, "-p", &proj, "up", "--detach", "--scale", "web=2"]);
	assert!(!out.status.success(), "scaling a fixed host port must fail");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains("only one container can bind a host port"),
		"expected port-conflict guidance, got: {stderr}"
	);
	// Clean up anything the failed attempt may have created.
	run(&["-f", c, "-p", &proj, "down"]);
}
