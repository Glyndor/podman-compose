//! What `build` tells you, and where.
//!
//! The build half of `reporting_contract.rs`, split out when that file reached
//! the line limit. Two promises: the image id lands on stdout when stdout is
//! not a terminal, so a script can read it; and in a pipe every buildah line
//! reaches stderr prefixed with the image tag, with the row's verbs in plain
//! form and no escape sequence anywhere (#1681).

mod harness;

use harness::{bin, podman_up};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

/// `build` writes its output to stderr, leaving stdout with only the new
/// image id. The id is dropped on a terminal where the row says it landed;
/// here, where stdout is a pipe (`Command::output` gives the child
/// one), the id is the only line on stdout and the buildah stream lives
/// on stderr.
///
/// It used `print!`, contradicting the documented promise that stdout stays
/// pipeable, the one thing a caller redirecting `podup build > log` relies on,
/// and the same promise `config` and `generate quadlet` are built around.
#[tokio::test]
async fn build_writes_the_image_id_to_stdout() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	fs::write(
		dir.path().join("Dockerfile"),
		"FROM alpine:latest\nRUN true\n",
	)
	.unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  tiny:\n    image: podup-build-probe:1\n    build:\n      context: .\n",
	)
	.unwrap();
	let out = Command::new(bin())
		.args([
			"-f",
			&compose.to_string_lossy(),
			"-p",
			&format!("t{}-bld", std::process::id()),
			"build",
		])
		.output()
		.expect("run podup build");
	let stdout = String::from_utf8_lossy(&out.stdout);
	let stderr = String::from_utf8_lossy(&out.stderr);
	let id_line = stdout
		.lines()
		.find(|line| !line.is_empty())
		.unwrap_or_else(|| panic!("build must print the image id on stdout; got:\n{stdout}"));
	// `podman images --format '{{.ID}}'` returns the short form (12 hex chars)
	// here; libpod's `ImageInspect` field `Id` is the full sha256 hex without
	// the prefix. Accept either; the script that reads this just wants the id.
	assert!(
		id_line.chars().all(|c| c.is_ascii_hexdigit()) && (12..=64).contains(&id_line.len()),
		"the printed line must be the image id: {id_line:?}"
	);
	assert!(
		!stderr.trim().is_empty(),
		"the build stream still goes to stderr: {stderr}"
	);
}

/// A piped build prefixes every stream line with `<image-tag> | ` and
/// closes with `Built`, in the same shape `logs` prefixes container output.
/// No board, no spinner, no cursor moves; animation in a CI log is a defect.
/// `Command::output` gives the child a pipe for stderr; that is the
/// condition being asserted.
#[tokio::test]
async fn a_piped_build_prefixes_every_stream_line_with_the_service() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	fs::write(
		dir.path().join("Dockerfile"),
		"FROM docker.io/library/alpine:3.20\nRUN echo hi\n",
	)
	.unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  tiny:\n    image: localhost/ux-pipe-build:1\n    build:\n      context: .\n",
	)
	.unwrap();
	let out = Command::new(bin())
		.args([
			"-f",
			&compose.to_string_lossy(),
			"-p",
			&format!("t{}-bpipe", std::process::id()),
			"build",
		])
		.output()
		.expect("run podup build");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		stderr.contains(" Image localhost/ux-pipe-build:1  Building"),
		"a piped build must report the working verb in plain form; got:\n{stderr}"
	);
	assert!(
		stderr.contains(" Image localhost/ux-pipe-build:1  Built"),
		"and the closing verb in plain form; got:\n{stderr}"
	);
	assert!(
		stderr.contains("localhost/ux-pipe-build:1 | STEP 1/2:"),
		"each stream line must carry the <image-tag> | prefix: {stderr}"
	);
	assert!(
		!stderr.contains('\x1b'),
		"a pipe gets no escape sequence at all, board or otherwise; got:\n{stderr:?}"
	);
	for marker in ["⠿", "✔", "[+] Running"] {
		assert!(
			!stderr.contains(marker),
			"a pipe gets no board furniture ({marker}); got:\n{stderr}"
		);
	}
}
