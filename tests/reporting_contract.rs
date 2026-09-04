//! Whether a command tells you what it did.
//!
//! The sibling of `output_contract.rs`: that file pins the *shape* of output,
//! this one pins that there is any. #1248 found six lifecycle commands exiting
//! 0 in complete silence on a project that was never created, a `push` that
//! uploaded an image and wrote zero bytes, an `up` that created three resources
//! and named none of them, and a `down` that announced destroying a data volume
//! which had never existed.
//!
//! Silence and a false report are the two failure modes, and each needs an
//! acceptance twin: a test that only asserts a note appears is equally
//! satisfied by a note that always appears.

mod harness;

use harness::{bin, podman_up, Project};
use std::fs;
use std::process::Command;
use tempfile::tempdir;

/// `push` says something. It used to say nothing at all.
///
/// Its only two user-facing lines were `tracing::info!`, and the CLI floors
/// tracing at `warn`, so a push that genuinely uploaded the image wrote zero
/// bytes to stdout and stderr and exited 0 — measured against a real local
/// registry, which afterwards listed the repository. This asserts the line is
/// back and that it goes to stderr, leaving stdout a clean pipe.
///
/// Deliberately pointed at a registry that cannot exist (port 1) with an image
/// that is not present locally: the progress line is emitted before the API
/// call, so the assertion needs no registry and no build, and the command still
/// fails afterwards — which is also what pins the line as *progress* rather
/// than a success message.
#[tokio::test]
async fn push_reports_the_image_it_is_pushing() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  x:\n    image: localhost:1/absent:1\n",
	)
	.unwrap();
	let out = Command::new(bin())
		.args([
			"-f",
			&compose.to_string_lossy(),
			"push",
			"--tls-verify=false",
		])
		.output()
		.expect("run podup push");
	let stderr = String::from_utf8_lossy(&out.stderr);
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stderr.contains("localhost:1/absent:1") && stderr.contains("Pushing"),
		"push must report the image on stderr; got stderr:\n{stderr}"
	);
	assert!(
		stdout.trim().is_empty(),
		"push must leave stdout a clean pipe; got stdout:\n{stdout}"
	);
}

/// `push --quiet` suppresses the progress lines. The flag existed while there
/// was no output for it to suppress, so nothing had ever exercised it.
#[tokio::test]
async fn push_quiet_suppresses_the_progress_lines() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  x:\n    image: localhost:1/absent:1\n",
	)
	.unwrap();
	let out = Command::new(bin())
		.args([
			"-f",
			&compose.to_string_lossy(),
			"push",
			"--quiet",
			"--tls-verify=false",
		])
		.output()
		.expect("run podup push --quiet");
	let stderr = String::from_utf8_lossy(&out.stderr);
	assert!(
		!stderr.contains("Pushing"),
		"--quiet must suppress the progress line; got stderr:\n{stderr}"
	);
}

/// `up` reports the networks and volumes it creates, the way `down` has always
/// reported removing them.
///
/// It did not: measured on a four-object project, `up` created `p4_default`,
/// `p4_extra` and `p4_data` and named none of them, while `down -v` on the same
/// project listed all three as `Removed`. Creation went through
/// `tracing::info!` under the CLI's `warn` floor, so the two halves of the same
/// lifecycle disagreed about whether resources were worth mentioning.
///
/// Asserted on a fresh project name, because the create is idempotent: a second
/// `up` correctly reports nothing, so a reused project would pass this whether
/// or not the control exists.
#[tokio::test]
async fn up_reports_the_networks_and_volumes_it_creates() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    \
		 volumes:\n      - data:/data\nvolumes:\n  data:\nnetworks:\n  extra:\n",
	)
	.unwrap();
	let compose = compose.to_string_lossy().into_owned();
	let name = format!("t{}-mk", std::process::id());
	let out = Command::new(bin())
		.args(["-f", &compose, "-p", &name, "up", "-d"])
		.output()
		.expect("run podup up");
	let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
	let _ = Command::new(bin())
		.args(["-f", &compose, "-p", &name, "down", "-v"])
		.output();
	for needle in [
		&format!("Network {name}_default"),
		&format!("Network {name}_extra"),
		&format!("Volume {name}_data"),
	] {
		assert!(
			stderr.contains(needle.as_str()) && stderr.contains("Created"),
			"up must report creating {needle}; got stderr:\n{stderr}"
		);
	}
}

/// `down` only reports removals that happened.
///
/// It reported three that had not: measured on a project name that had never
/// been created, `down -v` printed `Network …_extra Removed`, `Network
/// …_default Removed` and `Volume …_data Removed`. The cause was `delete_ok`
/// discarding the boolean `delete_existed` returns — the very distinction that
/// method exists to preserve, and which the container path had always used — so
/// a 404 reached the caller as `Ok(())` and was announced as a deletion.
///
/// A volume is where this is worst: it names data the operator is being told is
/// gone. The reference prints nothing at all here and exits 0.
#[tokio::test]
async fn down_does_not_report_removing_what_never_existed() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    volumes:\n      - data:/data\nvolumes:\n  \
		 data:\nnetworks:\n  extra:\n",
	)
	.unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-ghost", std::process::id()),
		_dir: dir,
	};
	let stderr = p.progress(&["down", "-v"]);
	assert!(
		!stderr.contains("Removed"),
		"nothing existed, so nothing may be reported as removed; got stderr:\n{stderr}"
	);
}

/// Every lifecycle command says so when it did nothing.
///
/// Measured before the fix on a project that was never created: `rm`, `stop`,
/// `restart`, `kill`, `pause` and `unpause` each printed zero lines and exited
/// 0, which reads exactly like success. Only `start` said anything. The cause is
/// that the per-replica query helpers (the bulk
/// [`Engine::live_project_replicas_sorted`] lookup used by `exec`/`cp`/
/// `port`/`logs`) fall back to the *static* compose names when nothing is
/// running, so each command dutifully walked a list of container names and
/// 404'd on every one of them.
#[tokio::test]
async fn idle_lifecycle_commands_say_they_did_nothing() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(&compose, "services:\n  web:\n    image: alpine:latest\n").unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-idle", std::process::id()),
		_dir: dir,
	};
	for (args, verb) in [
		(vec!["rm", "-f"], "remove"),
		(vec!["stop"], "stop"),
		(vec!["start"], "start"),
		(vec!["restart"], "restart"),
		(vec!["kill"], "signal"),
		(vec!["pause"], "pause"),
		(vec!["unpause"], "unpause"),
	] {
		let stderr = p.progress(&args);
		assert!(
			stderr.contains(&format!("no containers to {verb}")),
			"`{}` on an uncreated project must say it did nothing; got stderr:\n{stderr}",
			args.join(" ")
		);
	}
}

/// The acceptance twin of the test above: when the command really does act, the
/// note must not appear. Without this pairing, a `note_if_idle` that fired
/// unconditionally would satisfy the rejection test and prove nothing.
#[tokio::test]
async fn lifecycle_commands_stay_quiet_when_they_did_act() {
	if !podman_up().await {
		return;
	}
	let p = Project::start("acted");
	for (args, verb) in [
		(vec!["restart"], "restart"),
		(vec!["stop"], "stop"),
		(vec!["rm", "-f"], "remove"),
	] {
		let stderr = p.progress(&args);
		assert!(
			!stderr.contains(&format!("no containers to {verb}")),
			"`{}` acted, so it must not claim there was nothing to do; got stderr:\n{stderr}",
			args.join(" ")
		);
	}
}

/// `wait` names the container each exit code belongs to, one line per container,
/// and offers a machine path.
///
/// It printed a bare `0` per service. With more than one container nothing said
/// which code was whose, and a service scaled to three collapsed to one line.
/// Measured on docker compose v5.1.3 with three replicas: it reports per
/// container, so the granularity here follows the reference — the rendering
/// deliberately does not, since the reference prints a 64-character hex id, the
/// same sentence on every line, and nothing a parser can read.
#[tokio::test]
async fn wait_names_each_container_and_its_code() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  ok:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"exit 0\"]\n  \
		 bad:\n    image: alpine:latest\n    deploy:\n      replicas: 2\n    command: [\"sh\", \
		 \"-c\", \"exit 3\"]\n",
	)
	.unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-wait", std::process::id()),
		_dir: dir,
	};
	p.run(&["up", "-d"]);

	let table = p.run(&["wait"]);
	assert!(
		table.contains("NAME") && table.contains("EXIT"),
		"wait must print its header; got:\n{table}"
	);
	for needle in ["-ok-1", "-bad-1", "-bad-2"] {
		assert!(
			table.contains(needle),
			"every container gets its own line, including each replica; \
			 missing {needle} in:\n{table}"
		);
	}

	let ndjson = p.run(&["wait", "--format", "json"]);
	let rows: Vec<serde_json::Value> = ndjson
		.lines()
		.filter(|l| !l.trim().is_empty())
		.map(|l| serde_json::from_str(l).expect("each wait json line must parse on its own"))
		.collect();
	assert_eq!(rows.len(), 3, "one NDJSON object per container: {ndjson}");
	for row in &rows {
		assert!(row.get("Container").is_some(), "missing Container: {row}");
		assert!(row.get("ExitCode").is_some(), "missing ExitCode: {row}");
	}
	assert!(
		rows.iter().any(|r| r["ExitCode"] == 3),
		"the failing containers' code must survive: {ndjson}"
	);
}

/// A pipe gets the events, never the animation.
///
/// This is the contract that protects CI logs. The live region only exists when
/// stderr is a terminal and colour is on; anywhere else the same event model
/// comes out as append-only lines. **Animation in a CI log is a defect** — and
/// so is a CI log that says less than the terminal did, which is why the
/// intermediate transitions are asserted here too. Before the board they did
/// not exist at all: `up` reported only the finished state of each resource.
#[tokio::test]
async fn a_piped_up_gets_transitions_and_no_escapes() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-pipe", std::process::id()),
		_dir: dir,
	};
	let stderr = p.progress(&["up", "-d"]);
	assert!(
		!stderr.contains('\u{1b}'),
		"a pipe must get no escape sequences at all; got:\n{stderr:?}"
	);
	// Asserted per resource, not as "some line somewhere says Creating". A
	// looser version of this passed with the container's start event deleted,
	// because the network's own `Creating` satisfied it.
	let has = |name: &str, verbs: &[&str]| {
		stderr
			.lines()
			.any(|l| l.contains(name) && verbs.iter().any(|v| l.contains(v)))
	};
	let container = format!("{}-web-1", p.name);
	assert!(
		has(&container, &["Starting", "Creating"]),
		"the container needs its own transition, not only its ending; got:\n{stderr}"
	);
	assert!(
		has(&container, &["Started", "Created", "Running"]),
		"and it must still get its ending; got:\n{stderr}"
	);
	let network = format!("{}_default", p.name);
	assert!(
		has(&network, &["Creating"]),
		"the network needs one too; got:\n{stderr}"
	);
}

/// `--ansi never` means it. The board is gated on the colour choice as well as
/// on the terminal, because someone who asked for no escapes did not ask for a
/// quieter kind of escape.
#[tokio::test]
async fn ansi_never_gets_no_board() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(&compose, "services:\n  web:\n    image: alpine:latest\n").unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-noansi", std::process::id()),
		_dir: dir,
	};
	let stderr = p.progress(&["--ansi", "never", "up", "-d"]);
	assert!(
		!stderr.contains('\u{1b}'),
		"--ansi never must emit nothing to repaint with; got:\n{stderr:?}"
	);
}

/// The board writes to stderr only. stdout stays a clean pipe, which is what
/// lets `run -d` keep printing its container id there and `config` keep piping
/// into a file.
#[tokio::test]
async fn the_board_never_touches_stdout() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-stdout", std::process::id()),
		_dir: dir,
	};
	let stdout = p.run(&["up", "-d"]);
	assert!(
		stdout.trim().is_empty(),
		"up must leave stdout empty; got:\n{stdout}"
	);
}

/// `down` gets a board too, seeded from what Podman actually has rather than
/// from what the compose file describes — a service the file lists but that was
/// never created would sit on the board as a row that never moves.
#[tokio::test]
async fn a_piped_down_gets_transitions_for_what_exists() {
	if !podman_up().await {
		return;
	}
	let p = Project::start("dwn");
	let stderr = p.progress(&["down", "-v"]);
	assert!(
		!stderr.contains('\u{1b}'),
		"a pipe must get no escape sequences; got:\n{stderr:?}"
	);
	let has = |name: &str, verb: &str| stderr.lines().any(|l| l.contains(name) && l.contains(verb));
	let container = format!("{}-web-1", p.name);
	assert!(
		has(&container, "Stopping"),
		"the container needs its transition; got:\n{stderr}"
	);
	assert!(has(&container, "Removed"), "and its ending; got:\n{stderr}");
	let network = format!("{}_default", p.name);
	assert!(
		has(&network, "Removing") && has(&network, "Removed"),
		"the network needs both; got:\n{stderr}"
	);
}

/// `build` writes its output to stderr, leaving stdout with only the new
/// image id. The id is dropped on a terminal where the row says it landed;
/// here, where stdout is a pipe (`Command::output` gives the child
/// one), the id is the only line on stdout and the buildah stream lives
/// on stderr.
///
/// It used `print!`, contradicting the documented promise that stdout stays
/// pipeable — the one thing a caller redirecting `podup build > log` relies on,
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

/// `pull` reports through the progress layer, with a matching `Pulled`.
///
/// It used a bare `eprintln!` — the one user-facing line in the binary that
/// bypassed `ui` entirely, so it ignored `PROGRESS_ENABLED` and an embedder
/// asking podup to stay silent got it anyway. There was never a `Pulled` at all,
/// so a pull that finished looked exactly like one that hung.
#[tokio::test]
async fn pull_reports_both_ends() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(&compose, "services:\n  x:\n    image: alpine:latest\n").unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-pull", std::process::id()),
		_dir: dir,
	};
	let stderr = p.progress(&["pull"]);
	assert!(
		stderr.contains("Pulling") && stderr.contains("Pulled"),
		"pull must report starting and finishing; got:\n{stderr}"
	);
	assert_eq!(
		stderr.matches("Pulling").count(),
		1,
		"one image, one Pulling; got:\n{stderr}"
	);
}

/// One image, one `Pulling`, even on the `up` path.
///
/// `up` warms the image cache before the per-level walk and then pulls again
/// inside it, and both reported — so `Pulling` appeared twice per image on `up`
/// while a standalone `pull` printed it once. The prefetch is best-effort
/// warming and the walk's own pull is the authoritative one, so only the second
/// speaks.
///
/// `pull_policy: always` is what forces both stages to actually issue a pull for
/// an image that is already local; with the default `missing` the prefetch would
/// short-circuit and the duplication could not appear whether or not the control
/// exists.
#[tokio::test]
async fn up_pulls_an_image_once() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    pull_policy: always\n    command: \
		 [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-once", std::process::id()),
		_dir: dir,
	};
	let stderr = p.progress(&["up", "-d"]);
	let pulls = stderr.matches("Pulling").count();
	assert!(
		pulls <= 1,
		"one image must produce at most one Pulling on up; got {pulls} in:\n{stderr}"
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

/// A `pull` whose stderr is a pipe keeps the plain lines it always printed.
///
/// `pull` now opens the same board `up` does (#1671), and the board owns the
/// rendering once it is open. Off a terminal that must change nothing: the
/// events go out as ` Image <ref>  Pulling` / ` Pulled`, one line each, with no
/// cursor hiding, no cursor moves and no spinner. Animation in a CI log is a
/// defect, and so is a log that says less than it used to.
///
/// `Command::output` gives the child a pipe for stderr, which is exactly the
/// condition being asserted.
#[tokio::test]
async fn a_piped_pull_keeps_the_plain_lines() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(&compose, "services:\n  x:\n    image: alpine:latest\n").unwrap();
	let p = Project {
		compose: compose.to_string_lossy().into_owned(),
		name: format!("t{}-pipe", std::process::id()),
		_dir: dir,
	};
	let stderr = p.progress(&["pull"]);
	assert!(
		stderr.contains(" Image alpine:latest  Pulling"),
		"the plain line is unchanged in a pipe; got:\n{stderr}"
	);
	assert!(
		stderr.contains(" Image alpine:latest  Pulled"),
		"and so is its closing line; got:\n{stderr}"
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
