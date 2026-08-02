//! CLI binary integration tests (covers main.rs).
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

#[test]
fn cli_config_no_podman() {
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	fs::write(&compose, "services:\n  web:\n    image: alpine:latest\n").unwrap();

	let out = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "config"])
		.output()
		.expect("podup binary not found");

	assert!(
		out.status.success(),
		"config failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(stdout.contains("alpine"));
}

#[tokio::test]
async fn cli_up_and_down_via_binary() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let pid = std::process::id();
	let proj = format!("t{}-cli", pid);
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let up = Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"up",
			"--detach",
		])
		.output()
		.unwrap();
	assert!(
		up.status.success(),
		"up failed: {}",
		String::from_utf8_lossy(&up.stderr)
	);

	let down = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
	assert!(
		down.status.success(),
		"down failed: {}",
		String::from_utf8_lossy(&down.stderr)
	);
}

#[tokio::test]
async fn cli_ps_subcommand() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-clps", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"up",
			"--detach",
		])
		.output()
		.unwrap();

	let ps = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "ps"])
		.output()
		.unwrap();
	assert!(ps.status.success());
	// The exit code was the whole assertion here, on a command whose entire job
	// is what it prints. Reading STATUS matters especially: #590 was `ps` leaving
	// that column empty for every container, which an exit-code check cannot see.
	let out = String::from_utf8_lossy(&ps.stdout);
	assert!(
		out.contains(&format!("{proj}-web-1")),
		"ps did not list the running container: {out:?}"
	);
	assert!(
		out.contains("running"),
		"ps printed the container without its status: {out:?}"
	);

	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
}

#[tokio::test]
async fn cli_logs_subcommand() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-cllg", std::process::id());
	// The container has to say something. With `sleep infinity` it prints nothing,
	// so `logs` has no output to be right or wrong about and no assertion on it
	// could fail.
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo hello-from-web; sleep infinity\"]\n",
	)
	.unwrap();

	Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"up",
			"--detach",
		])
		.output()
		.unwrap();

	let logs = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "logs"])
		.output()
		.unwrap();
	assert!(logs.status.success());
	// Both halves of the line are a contract. The container's own output is the
	// point of the command, and the `service |` prefix was missing entirely once
	// (#594), which is invisible to a check on the exit code.
	let out = String::from_utf8_lossy(&logs.stdout);
	assert!(
		out.contains("hello-from-web"),
		"logs did not carry the container's output: {out:?}"
	);
	assert!(
		out.contains("web-1 |"),
		"logs dropped the per-service prefix: {out:?}"
	);

	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
}

#[tokio::test]
async fn cli_exec_subcommand() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-clex", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"up",
			"--detach",
		])
		.output()
		.unwrap();

	let exec = Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"exec",
			"web",
			"echo",
			"cli-exec",
		])
		.output()
		.unwrap();
	assert!(exec.status.success());
	// The command was chosen to print something and then nobody read it.
	let out = String::from_utf8_lossy(&exec.stdout);
	assert_eq!(
		out.trim(),
		"cli-exec",
		"exec did not forward the command's output"
	);

	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
}

#[tokio::test]
async fn cli_restart_subcommand() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-clrs", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"up",
			"--detach",
		])
		.output()
		.unwrap();

	let restart = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "restart"])
		.output()
		.unwrap();
	assert!(restart.status.success());
	// Progress goes to stderr, measured rather than assumed: asserting stdout here
	// would fail with nothing wrong. Six lifecycle commands used to exit 0 in
	// silence on a project that was never created, so naming the container is the
	// part worth pinning.
	let err = String::from_utf8_lossy(&restart.stderr);
	assert!(
		err.contains(&format!("{proj}-web-1")) && err.contains("Restarted"),
		"restart did not report which container it restarted: {err:?}"
	);

	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
}

#[tokio::test]
async fn cli_pull_subcommand() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	fs::write(&compose, "services:\n  web:\n    image: alpine:latest\n").unwrap();

	let pull = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "pull"])
		.output()
		.unwrap();
	assert!(pull.status.success());
	// Also stderr, measured. #1076 is the reason the reported outcome matters
	// here: a failed pull used to arrive as an in-band error line on a 200 and be
	// dropped, so `pull` exited 0 having pulled nothing.
	let err = String::from_utf8_lossy(&pull.stderr);
	assert!(
		err.contains("alpine:latest") && err.contains("Pulled"),
		"pull did not report pulling the image: {err:?}"
	);
}

#[tokio::test]
async fn cli_stop_and_start_subcommands() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-clss", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"up",
			"--detach",
		])
		.output()
		.unwrap();

	let stop = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "stop"])
		.output()
		.unwrap();
	assert!(stop.status.success(), "stop failed: {:?}", stop.stderr);

	let start = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "start"])
		.output()
		.unwrap();
	assert!(start.status.success(), "start failed: {:?}", start.stderr);

	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
}

#[tokio::test]
async fn cli_kill_subcommand() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-clkl", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"up",
			"--detach",
		])
		.output()
		.unwrap();

	let kill = Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"kill",
			"--signal",
			"SIGTERM",
		])
		.output()
		.unwrap();
	assert!(kill.status.success(), "kill failed: {:?}", kill.stderr);

	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
}

/// #758: `down` on a defined-but-never-created project is a clean, quiet no-op —
/// it must not synthesize predicted container names and leak a raw 404 / "could
/// not stop" warning.
#[tokio::test]
async fn cli_down_on_never_created_is_quiet_noop() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-dnvr", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	// `down` without a prior `up`: nothing exists, so this must exit 0 cleanly.
	let down = Command::new(bin())
		.env("RUST_LOG", "info")
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
	assert!(
		down.status.success(),
		"down on never-created failed: {}",
		String::from_utf8_lossy(&down.stderr)
	);
	let stderr = String::from_utf8_lossy(&down.stderr);
	assert!(
		!stderr.contains("404") && !stderr.contains("no such container"),
		"down leaked a 404 for a never-created project: {stderr}"
	);
	assert!(
		!stderr.contains("could not stop"),
		"down warned about stopping a phantom container: {stderr}"
	);
}

/// #758: `wait` on a defined-but-never-created service returns cleanly instead of
/// surfacing a raw `podman API error (HTTP 404)`.
#[tokio::test]
async fn cli_wait_on_never_created_is_quiet_noop() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-wnvr", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let wait = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "wait", "web"])
		.output()
		.unwrap();
	assert!(
		wait.status.success(),
		"wait on never-created failed: {}",
		String::from_utf8_lossy(&wait.stderr)
	);
	let stderr = String::from_utf8_lossy(&wait.stderr);
	assert!(
		!stderr.contains("404"),
		"wait leaked a 404 for a never-created service: {stderr}"
	);
}

/// #876: `stop` on a Created (never-started) container must not claim it was
/// "stopped" — it is a harmless no-op, so no "stopped" line is logged.
#[tokio::test]
async fn cli_stop_on_created_does_not_report_stopped() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-screa", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	// `create` builds the container but never starts it (state = Created).
	let create = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "create"])
		.output()
		.unwrap();
	assert!(
		create.status.success(),
		"create failed: {}",
		String::from_utf8_lossy(&create.stderr)
	);

	// `stop` with INFO logging on: a not-running container must not be reported
	// as "stopped".
	let stop = Command::new(bin())
		.env("RUST_LOG", "info")
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "stop"])
		.output()
		.unwrap();
	assert!(
		stop.status.success(),
		"stop failed: {}",
		String::from_utf8_lossy(&stop.stderr)
	);
	let stderr = String::from_utf8_lossy(&stop.stderr);
	assert!(
		!stderr.contains("stopped"),
		"stop claimed it stopped a not-running container: {stderr}"
	);

	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down"])
		.output()
		.unwrap();
}
