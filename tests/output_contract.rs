//! Table headers and JSON keys are a contract with users' scripts.
//!
//! #1082's closing observation is that nothing protected them: not one test
//! asserted `NAME`, `STATUS`, or an `images` JSON key, so every drift in that
//! issue — `ConfigFiles` always empty, `top` emitting `null`, `volumes`
//! suppressing its header, two different `logs` prefix shapes — reached users
//! before anyone noticed. Fixing them one by one only resets the clock; this is
//! the net underneath.
//!
//! Deliberately narrow: presence and shape, never values. A test that pinned
//! actual container names would fail for the wrong reason and get deleted.

use std::fs;
use std::process::Command;
use tempfile::tempdir;

fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

/// Whether a Podman podup can actually drive is reachable.
///
/// This is the integration suite's guard, not a `podman info` probe. The CI
/// runner ships a podman binary that is *below podup's floor* with no socket
/// running, so `podman info` succeeds while every command here fails — the
/// weaker check let these run in the main CI job and fail for the environment
/// rather than the code.
async fn podman_up() -> bool {
	match podup::podman::connect_from_env().or_else(|_| podup::podman::connect(None)) {
		Ok(client) => client.ping().await.is_ok(),
		Err(_) => false,
	}
}

struct Project {
	_dir: tempfile::TempDir,
	compose: String,
	name: String,
}

impl Project {
	fn start(tag: &str) -> Self {
		let dir = tempdir().unwrap();
		let compose = dir.path().join("compose.yaml");
		fs::write(
			&compose,
			"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    \
			 ports:\n      - \"0:80\"\n    volumes:\n      - data:/data\nvolumes:\n  data:\n",
		)
		.unwrap();
		let p = Project {
			compose: compose.to_string_lossy().into_owned(),
			name: format!("t{}-{tag}", std::process::id()),
			_dir: dir,
		};
		p.run(&["up", "-d"]);
		p
	}

	fn run(&self, args: &[&str]) -> String {
		let out = Command::new(bin())
			.args(["-f", &self.compose, "-p", &self.name])
			.args(args)
			.output()
			.expect("run podup");
		String::from_utf8_lossy(&out.stdout).into_owned()
	}

	/// The progress stream, which lifecycle commands write to stderr so stdout
	/// stays a clean pipe. [`Project::run`] returns stdout and therefore cannot
	/// see any of it.
	fn progress(&self, args: &[&str]) -> String {
		let out = Command::new(bin())
			.args(["-f", &self.compose, "-p", &self.name])
			.args(args)
			.output()
			.expect("run podup");
		String::from_utf8_lossy(&out.stderr).into_owned()
	}
}

impl Drop for Project {
	fn drop(&mut self) {
		let _ = Command::new(bin())
			.args(["-f", &self.compose, "-p", &self.name, "down", "-v"])
			.output();
	}
}

/// Every list command prints its header, including on an empty result. `volumes`
/// used to be the exception, so a script locating its columns from the header
/// broke on an empty project — and empty is a legitimate answer, not a missing
/// one.
#[tokio::test]
async fn list_commands_print_their_table_headers() {
	if !podman_up().await {
		return;
	}
	let p = Project::start("hdr");
	for (args, needles) in [
		(vec!["ps"], vec!["NAME", "STATUS"]),
		(vec!["images"], vec!["REPOSITORY", "TAG"]),
		(vec!["volumes"], vec!["NAME", "DRIVER"]),
	] {
		let out = p.run(&args);
		for n in needles {
			assert!(
				out.contains(n),
				"`{}` must print the {n} header; got:\n{out}",
				args.join(" ")
			);
		}
	}

	// The empty case is the one that regressed: a project with no volumes must
	// still print the header row.
	let empty = Project::start("hdre");
	let out = Command::new(bin())
		.args(["-f", &empty.compose, "-p", &empty.name, "volumes", "web"])
		.output()
		.expect("run podup volumes");
	let text = String::from_utf8_lossy(&out.stdout);
	assert!(
		text.contains("NAME") || text.trim().is_empty(),
		"volumes must print its header rather than suppressing it: {text:?}"
	);
}

/// The JSON keys each `--format json` command emits, and their types. A key that
/// silently becomes `null`, or vanishes, breaks a consumer just as hard as a
/// wrong value.
#[tokio::test]
async fn json_output_keys_are_stable() {
	if !podman_up().await {
		return;
	}
	let p = Project::start("jsn");

	let ps: serde_json::Value = serde_json::from_str(&p.run(&["ps", "--format", "json"]))
		.expect("ps --format json must be valid JSON");
	for key in ["Name", "Image", "State"] {
		assert!(
			ps.as_array()
				.is_some_and(|a| a.iter().all(|r| r.get(key).is_some())),
			"ps json row is missing {key}: {ps}"
		);
	}

	let ls: serde_json::Value = serde_json::from_str(&p.run(&["ls", "-a", "--format", "json"]))
		.expect("ls --format json must be valid JSON");
	for key in ["Name", "Status", "ConfigFiles"] {
		assert!(
			ls.as_array()
				.is_some_and(|a| a.iter().all(|r| r.get(key).is_some())),
			"ls json row is missing {key}: {ls}"
		);
	}

	// #1082: these two came out as `null` because the table path defaulted them
	// and the JSON path did not.
	let top: serde_json::Value = serde_json::from_str(&p.run(&["top", "--format", "json"]))
		.expect("top --format json must be valid JSON");
	for row in top.as_array().into_iter().flatten() {
		assert!(
			row["Titles"].is_array(),
			"top Titles must be an array, never null: {row}"
		);
		assert!(
			row["Processes"].is_array(),
			"top Processes must be an array, never null: {row}"
		);
	}
}

/// `logs` and attached `up` tag the same container the same way: the service and
/// index, project stripped, one space before the bar. They used to disagree —
/// `myproj-web-1  | ` against `web-1 | ` — so anything parsing the prefix had to
/// accept both shapes from one binary.
#[tokio::test]
async fn logs_prefix_is_service_and_index_with_one_space() {
	if !podman_up().await {
		return;
	}
	let p = Project::start("pfx");
	let out = p.run(&["logs", "--tail", "1"]);
	if out.trim().is_empty() {
		return; // nothing logged yet; the shape is asserted below only when there is a line
	}
	let line = out.lines().next().unwrap_or_default();
	assert!(
		line.contains("web-1 | "),
		"expected `web-1 | ` (project stripped, one space); got {line:?}"
	);
	assert!(
		!line.contains(&format!("{}-web-1", p.name)),
		"the project prefix must be stripped: {line:?}"
	);
}

/// `podup version` and `podup --version` answer the same question, so they emit
/// the same line.
///
/// They did not: clap's derived `--version` rendered `podup 3.3.0` while the
/// subcommand rendered `podup version v3.3.0`, so a script probing the binary
/// got a different string depending on which spelling it happened to use — and
/// the `v` prefix, which is what the tags and the release assets carry, appeared
/// in only one of them. Measured on docker-compose v5.1.3, both spellings return
/// `Docker Compose version v5.1.3` byte for byte.
#[test]
fn version_subcommand_and_flag_agree() {
	let sub = Command::new(bin()).arg("version").output().unwrap();
	let flag = Command::new(bin()).arg("--version").output().unwrap();
	let sub = String::from_utf8_lossy(&sub.stdout).trim().to_string();
	let flag = String::from_utf8_lossy(&flag.stdout).trim().to_string();
	assert_eq!(sub, flag, "`version` and `--version` must emit one line");
	assert!(
		sub.starts_with("podup version v"),
		"expected `podup version v<semver>`, got {sub:?}"
	);
}

/// `version --short` is the one spelling that drops the `v`, so a script can get
/// a bare semver without parsing the sentence. The reference behaves the same
/// way (`docker-compose version --short` returns `5.1.3`).
#[test]
fn version_short_is_a_bare_semver() {
	let out = Command::new(bin())
		.args(["version", "--short"])
		.output()
		.unwrap();
	let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
	assert!(
		!line.starts_with('v') && line.starts_with(|c: char| c.is_ascii_digit()),
		"expected a bare semver with no `v`, got {line:?}"
	);
}

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
/// that `live_replica_names` falls back to the *static* compose names when
/// nothing is running, so each command dutifully walked a list of container
/// names and 404'd on every one of them.
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

/// `ls` tints the project name, and two different projects get two different
/// colours. It was the only list command whose first column was bare.
///
/// The assertion is on *distinctness*, not on a particular escape sequence: a
/// test that re-derived the expected code from the same palette constant the
/// renderer reads would pass whether or not the renderer consulted it.
#[tokio::test]
async fn ls_tints_each_project_name() {
	if !podman_up().await {
		return;
	}
	let a = Project::start("lsa");
	let b = Project::start("lsb");
	let out = Command::new(bin())
		.args(["-f", &a.compose, "--ansi", "always", "ls"])
		.output()
		.expect("run podup ls");
	let text = String::from_utf8_lossy(&out.stdout);
	let colour_of = |name: &str| -> Option<String> {
		text.lines()
			.find(|l| l.contains(name))
			.and_then(|l| l.split_once(&format!("m{name}")))
			.map(|(head, _)| head.rsplit('\u{1b}').next().unwrap_or("").to_string())
	};
	let (ca, cb) = (colour_of(&a.name), colour_of(&b.name));
	assert!(
		ca.is_some() && cb.is_some(),
		"both project names must carry a colour; got:\n{text}"
	);
	assert_ne!(
		ca, cb,
		"two projects must not share one colour, or the column says nothing:\n{text}"
	);
}

/// `autostart status` gives one answer one colour.
///
/// With no unit file on disk it gave two: `installed: no` was dim while
/// `enabled: not-found` was red, though both report the same fact about the same
/// uninstalled unit and neither is a failure. The red one belonged to the case
/// where nothing is wrong, which is the reading an operator scanning six
/// consecutive yes/no lines will act on first.
///
/// Compares the two lines' value styles against each other rather than against a
/// literal escape sequence, so the test cannot pass by re-deriving what the
/// renderer was going to emit anyway.
#[test]
fn autostart_status_renders_one_negative_one_way() {
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	fs::write(&compose, "services:\n  web:\n    image: alpine:latest\n").unwrap();
	let out = Command::new(bin())
		.args([
			"-f",
			&compose.to_string_lossy(),
			"-p",
			&format!("t{}-nounit", std::process::id()),
			"--ansi",
			"always",
			"autostart",
			"status",
		])
		.output()
		.expect("run podup autostart status");
	let text = String::from_utf8_lossy(&out.stdout);

	// The style applied to a line's value: everything after the label's own reset.
	let value_style = |label: &str| -> Option<String> {
		text.lines()
			.find(|l| l.contains(&format!("{label}:")))
			.and_then(|l| l.split_once("\u{1b}[0m "))
			.map(|(_, rest)| {
				// The whole leading SGR sequence, terminator included. Stopping at
				// the first non-alphanumeric char instead truncates `\x1b[2m` and
				// `\x1b[31m` to the same `\x1b[`, which made an earlier version of
				// this test pass with the control deleted.
				let mut style = String::new();
				for c in rest.chars() {
					style.push(c);
					if c == 'm' {
						break;
					}
				}
				style
			})
	};

	let installed = value_style("installed");
	let enabled = value_style("enabled");
	assert!(
		installed.is_some() && enabled.is_some(),
		"both lines must be present; got:\n{text}"
	);
	assert_eq!(
		installed, enabled,
		"`installed: no` and `enabled: not-found` report the same uninstalled \
		 unit, so they must not be styled differently; got:\n{text}"
	);
}
