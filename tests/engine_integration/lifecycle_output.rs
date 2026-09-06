//! #817/#818/#873: the lifecycle commands must report concise per-container
//! progress on success (docker-compose style), on stderr so stdout stays a
//! clean pipe, and `run -d` must echo the started container's id on stdout.

use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

/// Write a one-service compose file (alpine, `pull_policy: never`, sleeping) and
/// return its directory + path.
fn sleeper() -> (tempfile::TempDir, std::path::PathBuf) {
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    pull_policy: never\n    \
		 command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	(dir, compose)
}

/// #817/#873: `up -d` prints a per-container `Container <name>  Started` line to
/// stderr and leaves stdout empty (so scripting/pipes are unaffected); `down`
/// reports each removal.
#[tokio::test]
async fn up_and_down_report_progress_on_stderr_clean_stdout() {
	if super::podman().await.is_none() {
		return;
	}
	let proj = proj("lout-ud");
	let (_dir, compose) = sleeper();
	let cf = compose.to_str().unwrap();

	let up = Command::new(bin())
		.args(["-f", cf, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();
	assert!(
		up.status.success(),
		"up failed: {}",
		String::from_utf8_lossy(&up.stderr)
	);
	let out = String::from_utf8_lossy(&up.stdout);
	let err = String::from_utf8_lossy(&up.stderr);
	assert!(
		out.trim().is_empty(),
		"up -d must keep stdout clean for scripting, got: {out:?}"
	);
	assert!(
		err.contains(&format!("Container {proj}-web")) && err.contains("Started"),
		"up -d must report the started container on stderr, got: {err:?}"
	);

	let down = Command::new(bin())
		.args(["-f", cf, "-p", &proj, "down"])
		.output()
		.unwrap();
	assert!(
		down.status.success(),
		"down failed: {}",
		String::from_utf8_lossy(&down.stderr)
	);
	let derr = String::from_utf8_lossy(&down.stderr);
	assert!(
		derr.contains(&format!("Container {proj}-web")) && derr.contains("Removed"),
		"down must report the removed container on stderr, got: {derr:?}"
	);
}

/// #818: `start` on a project that was never created prints a clear
/// "nothing to do" message (to stderr) and still exits 0.
#[tokio::test]
async fn start_on_never_created_reports_nothing_to_do() {
	if super::podman().await.is_none() {
		return;
	}
	let proj = proj("lout-cold");
	let (_dir, compose) = sleeper();

	let start = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "start"])
		.output()
		.unwrap();
	assert!(
		start.status.success(),
		"start failed: {}",
		String::from_utf8_lossy(&start.stderr)
	);
	let err = String::from_utf8_lossy(&start.stderr);
	assert!(
		err.contains("no containers to start"),
		"cold start must report nothing to do, got: {err:?}"
	);
}

/// #817: `run -d` echoes the started container's name to stdout (like
/// `docker compose run -d`), so a script can capture an id.
#[tokio::test]
async fn run_detached_echoes_container_name_to_stdout() {
	if super::podman().await.is_none() {
		return;
	}
	let proj = proj("lout-rund");
	let (_dir, compose) = sleeper();
	let cf = compose.to_str().unwrap();

	let run = Command::new(bin())
		.args(["-f", cf, "-p", &proj, "run", "-d", "web", "sleep", "30"])
		.output()
		.unwrap();
	assert!(
		run.status.success(),
		"run -d failed: {}",
		String::from_utf8_lossy(&run.stderr)
	);
	let out = String::from_utf8_lossy(&run.stdout);
	assert!(
		out.contains(&format!("{proj}-web-run-")),
		"run -d must echo the container name on stdout, got: {out:?}"
	);

	// Clean up the detached run container plus any project state.
	let _ = Command::new(bin())
		.args(["-f", cf, "-p", &proj, "down"])
		.output();
}
