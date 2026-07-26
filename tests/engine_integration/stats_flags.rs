//! CLI integration tests for `stats` table-shaping flags (`--all`, `--no-trunc`,
//! `--format`). Kept in its own submodule so `cli_commands.rs` stays within the
//! source line limit.
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn cli_stats_flags_truncate_all_and_format() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	// A service name long enough that `{proj}-{svc}-1` overflows the 32-char NAME
	// column, so truncation/`--no-trunc` is observable.
	let svc = "superlongservicenamethatoverflows";
	let proj = format!("t{}-stf", std::process::id());
	fs::write(
		&compose,
		format!(
			"services:\n  {svc}:\n    image: alpine:latest\n    pull_policy: never\n    command: [\"sleep\", \"infinity\"]\n"
		),
	)
	.unwrap();
	let c = compose.to_str().unwrap();
	// A container is always index-suffixed `{proj}-{svc}-1`, even at one replica.
	let full = format!("{proj}-{svc}-1");

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();

	// `--no-trunc` shows the full container name; the default table truncates it
	// (ellipsis present, full name gone).
	let notrunc = run(c, &proj, &["stats", "--no-stream", "--no-trunc"]);
	assert!(
		notrunc.contains(&full),
		"--no-trunc must show full name: {notrunc}"
	);

	let truncd = run(c, &proj, &["stats", "--no-stream"]);
	assert!(
		truncd.contains('…'),
		"default table must truncate: {truncd}"
	);
	assert!(
		!truncd.contains(&full),
		"default must not show full name: {truncd}"
	);

	// `--format json` emits a JSON array with the full (untruncated) name.
	let json = run(c, &proj, &["stats", "--no-stream", "--format", "json"]);
	assert!(
		json.contains("\"Name\""),
		"json must have Name field: {json}"
	);
	assert!(
		json.contains("\"CPUPerc\""),
		"json must have CPUPerc: {json}"
	);
	assert!(json.contains(&full), "json must carry full name: {json}");

	// Stop the container: it drops out of the default (running-only) view but
	// `--all` folds it back in as a zeroed row.
	Command::new(bin())
		.args(["-f", c, "-p", &proj, "stop"])
		.output()
		.unwrap();
	let stopped = run(c, &proj, &["stats", "--no-stream", "--no-trunc"]);
	assert!(
		!stopped.contains(&full),
		"stopped container must be hidden: {stopped}"
	);
	let all = run(c, &proj, &["stats", "--no-stream", "--all", "--no-trunc"]);
	assert!(
		all.contains(&full),
		"--all must include stopped container: {all}"
	);

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "down"])
		.output()
		.unwrap();
}

/// Run the built `podup` against compose file `c`/project `proj` with `args` and
/// return its stdout, asserting a clean exit.
fn run(c: &str, proj: &str, args: &[&str]) -> String {
	let mut full = vec!["-f", c, "-p", proj];
	full.extend_from_slice(args);
	let out = Command::new(bin()).args(&full).output().unwrap();
	assert!(
		out.status.success(),
		"podup {args:?} failed: {:?}",
		out.stderr
	);
	String::from_utf8_lossy(&out.stdout).into_owned()
}

/// #1082: `stats --format json` while streaming emitted one pretty-printed array
/// per sampling frame, concatenated. That is neither a single JSON document nor
/// NDJSON, so no parser accepts it — the machine-readable format was unreadable
/// by machines for as long as it streamed.
///
/// Streaming now emits one compact array per line. `--no-stream` prints a single
/// frame and exits, so it stays a pretty document, which is valid on its own.
#[tokio::test]
async fn cli_stats_streaming_json_is_ndjson() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-ndj", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();
	let run = |args: &[&str]| {
		Command::new(bin())
			.args(["-f", c, "-p", &proj])
			.args(args)
			.output()
			.expect("run podup")
	};
	run(&["up", "-d"]);

	// Take a couple of frames, then stop: the stream never ends on its own.
	let out = Command::new("timeout")
		.args([
			"4",
			bin(),
			"-f",
			c,
			"-p",
			&proj,
			"stats",
			"--format",
			"json",
		])
		.output()
		.expect("run podup stats");
	let text = String::from_utf8_lossy(&out.stdout);
	let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
	assert!(!lines.is_empty(), "no stats frames were emitted");
	for line in &lines {
		serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
			panic!("streaming frame is not valid JSON on its own ({e}): {line}")
		});
	}

	// `--no-stream` stays a single valid document.
	let one = run(&["stats", "--no-stream", "--format", "json"]);
	serde_json::from_slice::<serde_json::Value>(&one.stdout)
		.expect("--no-stream must emit one valid JSON document");

	run(&["down", "-v"]);
}

/// #1082: `port` with no host binding printed a blank line and exited 0, so
/// `HOST=$(podup port web 80)` yielded an empty string with a success status and
/// a script could not tell "not published" from "published at ''".
#[tokio::test]
async fn cli_port_without_a_binding_exits_nonzero() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-prt", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();
	let run = |args: &[&str]| {
		Command::new(bin())
			.args(["-f", c, "-p", &proj])
			.args(args)
			.output()
			.expect("run podup")
	};
	run(&["up", "-d"]);

	let out = run(&["port", "web", "80"]);
	assert!(
		!out.status.success(),
		"an unpublished port must not report success: stdout={:?}",
		String::from_utf8_lossy(&out.stdout)
	);

	run(&["down", "-v"]);
}

/// The published-port case, which only the CLI can check: `Engine::port` returns
/// `Result<()>` and writes the binding to stdout, so the engine-level test can
/// assert nothing about the binding its own name promises.
#[tokio::test]
async fn cli_port_prints_the_published_binding() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-prtb", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    ports:\n      - \"127.0.0.1:18081:80\"\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();
	let run = |args: &[&str]| {
		Command::new(bin())
			.args(["-f", c, "-p", &proj])
			.args(args)
			.output()
			.expect("run podup")
	};
	run(&["up", "-d"]);

	let out = run(&["port", "web", "80"]);
	let stdout = String::from_utf8_lossy(&out.stdout).to_string();
	let status = out.status;
	run(&["down", "-v"]);

	assert!(
		status.success(),
		"port failed for a published port: {stdout:?}"
	);
	assert!(
		stdout.contains("127.0.0.1:18081"),
		"port did not print the host binding it was asked for: {stdout:?}"
	);
}
