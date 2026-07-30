//! Niche-command CLI integration tests (wait/export/commit), split for the
//! source line limit.
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

fn run(args: &[&str]) -> std::process::Output {
	Command::new(bin()).args(args).output().unwrap()
}
#[tokio::test]
async fn cli_wait_names_the_container_and_its_exit_code() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-wait", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  job:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"exit 0\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run(&["-f", c, "-p", &proj, "up", "-d"]);
	let out = run(&["-f", c, "-p", &proj, "wait", "job"]);
	assert!(out.status.success(), "wait failed: {:?}", out.stderr);
	// One line per container, naming it (#1248). This used to assert a line
	// that was exactly `"0"` — a bare code with nothing saying whose it was,
	// which is what the change replaced.
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout
			.lines()
			.any(|l| l.contains(&format!("{proj}-job-1")) && l.trim().ends_with('0')),
		"wait must name the container and its exit code; got:\n{stdout}"
	);
	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_export_writes_tar() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-export", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();
	let tar = dir.path().join("rootfs.tar");

	run(&["-f", c, "-p", &proj, "up", "-d"]);
	let out = run(&[
		"-f",
		c,
		"-p",
		&proj,
		"export",
		"-o",
		tar.to_str().unwrap(),
		"web",
	]);
	run(&["-f", c, "-p", &proj, "down"]);
	assert!(out.status.success(), "export failed: {:?}", out.stderr);
	let meta = fs::metadata(&tar).expect("export must create the tar file");
	assert!(meta.len() > 0, "exported tar must be non-empty");
}

#[tokio::test]
async fn cli_commit_creates_image() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-commit", std::process::id());
	let img = format!("podup-commit-test-{}:latest", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run(&["-f", c, "-p", &proj, "up", "-d"]);
	let out = run(&["-f", c, "-p", &proj, "commit", "web", &img]);
	run(&["-f", c, "-p", &proj, "down"]);
	let exists = std::process::Command::new("podman")
		.args(["image", "exists", &img])
		.status()
		.map(|s| s.success())
		.unwrap_or(false);
	// Clean up the committed image regardless.
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", &img])
		.output();
	assert!(out.status.success(), "commit failed: {:?}", out.stderr);
	assert!(exists, "commit must create the target image");
}

#[tokio::test]
async fn cli_attach_streams_output_until_exit() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-attach", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	// `attach` streams LIVE output only (libpod `tail=0`), like `docker attach`:
	// anything emitted before the client connects is not replayed. So the
	// container must keep emitting after start, or the test races the attach
	// connection. It prints a line every second for a few seconds, then exits —
	// attach connects, catches a live line, and the follow stream closes when the
	// container stops (attach returns rather than blocking forever).
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"for i in 1 2 3 4 5; do echo attached-hi; sleep 1; done\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run(&["-f", c, "-p", &proj, "up", "-d"]);
	let out = run(&["-f", c, "-p", &proj, "attach", "web"]);
	run(&["-f", c, "-p", &proj, "down"]);
	assert!(out.status.success(), "attach failed: {:?}", out.stderr);
	assert!(
		String::from_utf8_lossy(&out.stdout).contains("attached-hi"),
		"attach must stream the container's live output"
	);
}

#[tokio::test]
async fn engine_events_stream_connects() {
	let client = match super::podman().await {
		Some(d) => d,
		None => return,
	};
	let engine = Engine::new(client, proj("events"));
	// The stream runs until interrupted; bound it with a short timeout. A
	// timeout (not an error) means the event stream connected and stayed open.
	let res = tokio::time::timeout(
		std::time::Duration::from_millis(800),
		engine.stream_events(true),
	)
	.await;
	assert!(
		res.is_err(),
		"events stream should stay open until interrupted"
	);
}
