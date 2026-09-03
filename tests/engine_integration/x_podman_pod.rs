//! `x-podman-pod: true` against a real Podman: one pod per project, every
//! service inside it, service names resolving to the shared namespace, a
//! pod recreate when the published ports change, and a clean `down`.
use std::fs;
use std::process::Command;

use super::*;

fn write_compose(dir: &std::path::Path, ports_for_db: &str) -> String {
	let yaml = format!(
		"x-podman-pod: true\nservices:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  db:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"while true; do echo hi | nc -l -p 5432; done\"]\n{ports_for_db}"
	);
	let path = dir.path_buf().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	path.to_str().unwrap().to_string()
}

trait PathBufExt {
	fn path_buf(&self) -> std::path::PathBuf;
}
impl PathBufExt for std::path::Path {
	fn path_buf(&self) -> std::path::PathBuf {
		self.to_path_buf()
	}
}

fn pod_json(project: &str) -> Option<serde_json::Value> {
	let out = Command::new("podman")
		.args(["pod", "inspect", project])
		.output()
		.ok()?;
	if !out.status.success() {
		return None;
	}
	let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
	// Podman 5 returns an object, Podman 6 an array with one object.
	Some(v.as_array().and_then(|a| a.first().cloned()).unwrap_or(v))
}

fn down(compose: &str, proj: &str) {
	let _ = Command::new(bin())
		.args(["-f", compose, "-p", proj, "down", "-v"])
		.output();
}

#[tokio::test]
async fn up_with_pod_creates_one_pod_with_every_service() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("pod-one");
	let compose = write_compose(dir.path(), "");
	let out = Command::new(bin())
		.args(["-f", &compose, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	// The progress stream goes to stderr; the two are read together so the
	// assertion does not depend on which one carries a given line.
	let stdout = format!(
		"{}{}",
		String::from_utf8_lossy(&out.stdout),
		String::from_utf8_lossy(&out.stderr)
	);
	let pod = pod_json(&proj);
	down(&compose, &proj);
	assert!(out.status.success(), "up failed:\n{stdout}");
	let pod = pod.expect("the project pod must exist after up");
	assert_eq!(pod["Name"], proj);
	let shared: Vec<String> = pod["SharedNamespaces"]
		.as_array()
		.map(|a| {
			a.iter()
				.filter_map(|v| v.as_str().map(str::to_string))
				.collect()
		})
		.unwrap_or_default();
	assert_eq!(
		shared,
		vec!["net".to_string()],
		"only the network namespace is shared: {pod}"
	);
	let members: Vec<String> = pod["Containers"]
		.as_array()
		.map(|a| {
			a.iter()
				.filter_map(|c| c["Name"].as_str().map(str::to_string))
				.collect()
		})
		.unwrap_or_default();
	assert!(
		members.iter().any(|m| m == &format!("{proj}-web-1"))
			&& members.iter().any(|m| m == &format!("{proj}-db-1")),
		"both services must be members of the pod: {members:?}"
	);
	assert!(
		stdout.contains("Pod") && stdout.contains("Created"),
		"up must report the pod it created:\n{stdout}"
	);
}

#[tokio::test]
async fn services_in_a_pod_resolve_each_other_to_localhost() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("pod-dns");
	let compose = write_compose(dir.path(), "");
	let up = Command::new(bin())
		.args(["-f", &compose, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let resolved = Command::new("podman")
		.args(["exec", &format!("{proj}-web-1"), "getent", "hosts", "db"])
		.output()
		.unwrap();
	let reached = Command::new("podman")
		.args([
			"exec",
			&format!("{proj}-web-1"),
			"nc",
			"-z",
			"-w",
			"3",
			"db",
			"5432",
		])
		.output()
		.unwrap();
	down(&compose, &proj);
	assert!(
		up.status.success(),
		"up failed: {}",
		String::from_utf8_lossy(&up.stderr)
	);
	let hosts = String::from_utf8_lossy(&resolved.stdout).to_string();
	assert!(
		hosts.starts_with("127.0.0.1"),
		"db must resolve to the shared namespace, got: {hosts:?}"
	);
	assert!(
		reached.status.success(),
		"a TCP connect to db:5432 by name must succeed inside the pod: {}",
		String::from_utf8_lossy(&reached.stderr)
	);
}

#[tokio::test]
async fn a_port_change_recreates_the_pod() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("pod-port");
	let compose = write_compose(dir.path(), "");
	let first = Command::new(bin())
		.args(["-f", &compose, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let id_before = pod_json(&proj).and_then(|p| p["Id"].as_str().map(str::to_string));
	let compose2 = write_compose(dir.path(), "    ports:\n      - \"127.0.0.1:0:5432\"\n");
	let second = Command::new(bin())
		.args(["-f", &compose2, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let id_after = pod_json(&proj).and_then(|p| p["Id"].as_str().map(str::to_string));
	let stdout = format!(
		"{}{}",
		String::from_utf8_lossy(&second.stdout),
		String::from_utf8_lossy(&second.stderr)
	);
	down(&compose2, &proj);
	assert!(
		first.status.success(),
		"first up failed: {}",
		String::from_utf8_lossy(&first.stderr)
	);
	assert!(
		second.status.success(),
		"second up failed: {}",
		String::from_utf8_lossy(&second.stderr)
	);
	assert!(
		id_before.is_some() && id_after.is_some(),
		"the pod must exist after both ups"
	);
	assert_ne!(
		id_before, id_after,
		"a changed port set must recreate the pod"
	);
	assert!(
		stdout.contains("Pod") && stdout.contains("Recreated"),
		"the recreate must be reported with the Recreated vocabulary:\n{stdout}"
	);
	assert!(
		!stdout.contains("orphan"),
		"the pod's infra container is not an orphan of the project:\n{stdout}"
	);
}

#[tokio::test]
async fn down_removes_the_pod() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("pod-down");
	let compose = write_compose(dir.path(), "");
	let up = Command::new(bin())
		.args(["-f", &compose, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let existed = pod_json(&proj).is_some();
	let out = Command::new(bin())
		.args(["-f", &compose, "-p", &proj, "down"])
		.output()
		.unwrap();
	let gone = pod_json(&proj).is_none();
	assert!(
		up.status.success(),
		"up failed: {}",
		String::from_utf8_lossy(&up.stderr)
	);
	assert!(existed, "the pod must exist before down");
	assert!(
		out.status.success(),
		"down failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	assert!(gone, "down must remove the pod");
}

/// A `userns_mode` every service declares is applied to the pod, so a member
/// runs in that user namespace without carrying the key itself.
#[tokio::test]
async fn a_pod_takes_the_services_user_namespace() {
	if podman().await.is_none() {
		return;
	}
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("pod-userns");
	let yaml = "x-podman-pod: true\nservices:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    userns_mode: auto\n  db:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    userns_mode: auto\n";
	let path = dir.path().join("docker-compose.yml");
	fs::write(&path, yaml).unwrap();
	let compose = path.to_str().unwrap().to_string();
	let up = Command::new(bin())
		.args(["-f", &compose, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	let in_pod = Command::new("podman")
		.args([
			"exec",
			&format!("{proj}-web-1"),
			"cat",
			"/proc/self/uid_map",
		])
		.output()
		.unwrap();
	let plain = Command::new("podman")
		.args(["run", "--rm", "alpine:latest", "cat", "/proc/self/uid_map"])
		.output()
		.unwrap();
	down(&compose, &proj);
	assert!(
		up.status.success(),
		"up failed: {}",
		String::from_utf8_lossy(&up.stderr)
	);
	let in_pod = String::from_utf8_lossy(&in_pod.stdout).to_string();
	let plain = String::from_utf8_lossy(&plain.stdout).to_string();
	assert_ne!(
		in_pod.trim(),
		plain.trim(),
		"a member of a pod created with userns auto must not run in the default rootless mapping"
	);
}
