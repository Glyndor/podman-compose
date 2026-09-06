//! CLI tests for output the library-level tests cannot reach.
//!
//! `logs`, `top` and an attached `up` all write to stdout and return
//! `Result<()>`, so their engine-level counterparts can only assert the absence
//! of an error. These drive the binary, where the bytes are readable. Split out
//! of `cli_commands.rs` when the additions took it past the 500 code-line limit.
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

/// `logs_scaled_service_all_replicas` promises every replica is included and
/// cannot check it. This does. The regression it guards is real history: #592
/// was by-service commands failing to resolve replicas after a scale.
#[tokio::test]
async fn cli_logs_covers_every_replica_of_a_scaled_service() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-lgscale", std::process::id());
	// Each replica prints, so a `logs` that reached only the first one is visible
	// as a missing prefix rather than as shorter output nobody counts.
	fs::write(
		&compose,
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo hello-from-worker; sleep infinity\"]\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "--detach"])
		.output()
		.unwrap();
	let logs = Command::new(bin())
		.args(["-f", c, "-p", &proj, "logs"])
		.output()
		.unwrap();
	Command::new(bin())
		.args(["-f", c, "-p", &proj, "down"])
		.output()
		.unwrap();

	assert!(logs.status.success(), "logs failed: {:?}", logs.stderr);
	let out = String::from_utf8_lossy(&logs.stdout);
	assert!(
		out.contains("worker-1 | hello-from-worker"),
		"logs missed the first replica: {out:?}"
	);
	assert!(
		out.contains("worker-2 | hello-from-worker"),
		"logs stopped at the first replica: {out:?}"
	);
}

/// The `top` half of the same gap: `cli_top_subcommand` drives one service, so
/// "all replicas" was asserted nowhere.
#[tokio::test]
async fn cli_top_covers_every_replica_of_a_scaled_service() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-tpscale", std::process::id());
	fs::write(
		&compose,
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "--detach"])
		.output()
		.unwrap();
	let top = Command::new(bin())
		.args(["-f", c, "-p", &proj, "top"])
		.output()
		.unwrap();
	Command::new(bin())
		.args(["-f", c, "-p", &proj, "down"])
		.output()
		.unwrap();

	assert!(top.status.success(), "top failed: {:?}", top.stderr);
	let out = String::from_utf8_lossy(&top.stdout);
	assert!(
		out.contains(&format!("{proj}-worker-1")),
		"top missed the first replica: {out:?}"
	);
	assert!(
		out.contains(&format!("{proj}-worker-2")),
		"top stopped at the first replica: {out:?}"
	);
}

/// `logs_with_stderr_output` names the stderr path and cannot read it. Measured
/// while writing this: podup keeps the streams apart, so the container's stdout
/// reaches podup's stdout and its stderr reaches podup's stderr, both carrying
/// the service prefix. That separation is the contract, and it is finer than
/// "the line appears somewhere": folding stderr into stdout would satisfy a
/// laxer check while breaking `podup logs 2>/dev/null` for anyone filtering.
#[tokio::test]
async fn cli_logs_keeps_container_stdout_and_stderr_apart() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-lgerr", std::process::id());
	fs::write(
		&compose,
		"services:\n  noisy:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo on-stdout; echo on-stderr 1>&2; sleep infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "--detach"])
		.output()
		.unwrap();
	let logs = Command::new(bin())
		.args(["-f", c, "-p", &proj, "logs"])
		.output()
		.unwrap();
	Command::new(bin())
		.args(["-f", c, "-p", &proj, "down"])
		.output()
		.unwrap();

	assert!(logs.status.success(), "logs failed: {:?}", logs.stderr);
	let out = String::from_utf8_lossy(&logs.stdout);
	let err = String::from_utf8_lossy(&logs.stderr);
	assert!(
		out.contains("noisy-1 | on-stdout"),
		"the container's stdout did not reach podup's stdout: {out:?}"
	);
	assert!(
		err.contains("noisy-1 | on-stderr"),
		"the container's stderr did not reach podup's stderr: {err:?}"
	);
	assert!(
		!out.contains("on-stderr"),
		"stderr was folded into stdout, so `podup logs 2>/dev/null` would carry it: {out:?}"
	);
}

/// The wire-format correction from #1365: the libpod log stream is the 8-byte
/// `[stream_type][3 pad bytes][size_big_endian: u32]` frame Docker uses, and a
/// `logs` invocation must demultiplex both streams from a real daemon. Reading
/// just the visible line is the easy half; checking the service prefix is the
/// harder half and the one the parser only gets right when the size field is
/// interpreted as a `u32`, not the four bytes it would have read as before.
#[tokio::test]
async fn cli_logs_demuxes_eight_byte_frames_from_a_real_daemon() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-lgframe", std::process::id());
	fs::write(
		&compose,
		"services:\n  chatty:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"printf 'on-stdout\\\\n'; printf 'on-stderr\\\\n' 1>&2; sleep infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "--detach"])
		.output()
		.unwrap();
	let logs = Command::new(bin())
		.args(["-f", c, "-p", &proj, "logs"])
		.output()
		.unwrap();
	Command::new(bin())
		.args(["-f", c, "-p", &proj, "down"])
		.output()
		.unwrap();

	assert!(logs.status.success(), "logs failed: {:?}", logs.stderr);
	let out = String::from_utf8_lossy(&logs.stdout);
	let err = String::from_utf8_lossy(&logs.stderr);
	assert!(
		out.contains("chatty-1 | on-stdout"),
		"a stdout frame landed somewhere other than podup's stdout: {out:?}"
	);
	assert!(
		err.contains("chatty-1 | on-stderr"),
		"a stderr frame landed somewhere other than podup's stderr: {err:?}"
	);
}
/// An attached `up` (no `--detach`) is where that content is reachable.
#[tokio::test]
async fn cli_attached_up_carries_the_container_output() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-attach", std::process::id());
	// The command exits on its own, so the attached `up` returns without needing
	// a signal, with no timeout standing in for synchronisation.
	fs::write(
		&compose,
		"services:\n  chatty:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo attached-output; sleep 2\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let up = Command::new(bin())
		.args(["-f", c, "-p", &proj, "up"])
		.output()
		.unwrap();
	Command::new(bin())
		.args(["-f", c, "-p", &proj, "down"])
		.output()
		.unwrap();

	assert!(up.status.success(), "attached up failed: {:?}", up.stderr);
	let out = String::from_utf8_lossy(&up.stdout);
	assert!(
		out.contains("chatty-1 | attached-output"),
		"the attached up did not carry the container's output: {out:?}"
	);
}
