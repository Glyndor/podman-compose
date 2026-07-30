//! CLI-surface checks for the flag-parity sweep: interspersed flags after a
//! service positional, value validation, conflicting-flag rejection, the
//! tolerant `help` handler, non-zero exits for missing subcommands, and the new
//! per-command flags. These exercise the built binary's argument parsing and
//! exit codes; none of them contact Podman (they either stop at `--help`, fail
//! parsing, or reach runtime against a nonexistent socket).

use std::fs;
use std::process::{Command, Output};

fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

/// True when clap rejected the arguments itself (exit 2 with the `error:`/
/// `Usage:` banner) rather than parsing and failing later at runtime.
fn is_clap_usage_error(out: &Output) -> bool {
	let stderr = String::from_utf8_lossy(&out.stderr);
	out.status.code() == Some(2) && stderr.contains("error:") && stderr.contains("Usage:")
}

/// True when clap rejected a value (exit 2 with an `error:` line). Value-parser
/// errors print the offending value and a `--help` hint but no `Usage:` banner.
fn is_clap_value_error(out: &Output) -> bool {
	let stderr = String::from_utf8_lossy(&out.stderr);
	out.status.code() == Some(2) && stderr.contains("error:")
}

/// Run podup against a nonexistent socket so a parsed command reaches runtime
/// (and fails connecting) instead of being rejected by clap.
fn run_offline(args: &[&str]) -> Output {
	let mut full = vec!["--socket", "/nonexistent.sock"];
	full.extend_from_slice(args);
	Command::new(bin())
		.args(&full)
		.output()
		.expect("run podup offline")
}

fn help_of(cmd: &str) -> String {
	let out = Command::new(bin())
		.args([cmd, "--help"])
		.output()
		.expect("run <cmd> --help");
	assert!(out.status.success(), "`{cmd} --help` failed");
	String::from_utf8_lossy(&out.stdout).into_owned()
}

// --- #717: flags after a service positional are no longer swallowed ----------

#[test]
fn flags_after_service_positional_parse() {
	// Each of these used to capture the trailing flag as a phantom service.
	for args in [
		&["pull", "web", "--ignore-pull-failures"][..],
		&["push", "web", "--ignore-push-failures"][..],
		&["kill", "web", "-s", "SIGTERM"][..],
		&["wait", "web", "--help"][..],
		&["volumes", "web", "--quiet"][..],
	] {
		let out = run_offline(args);
		assert!(
			!is_clap_usage_error(&out),
			"`{args:?}` should parse interspersed flags; got:\n{}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
}

// --- #747: invalid pull policy rejected at parse time ------------------------

#[test]
fn invalid_pull_policy_is_rejected() {
	for args in [
		&["pull", "--policy", "bogus"][..],
		&["up", "--pull", "nope"][..],
	] {
		let out = run_offline(args);
		assert!(
			is_clap_value_error(&out),
			"`{args:?}` should be a parse error; got:\n{}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
	// A valid value parses.
	assert!(!is_clap_value_error(&run_offline(&[
		"pull", "--policy", "always", "web"
	])));
}

// --- #840/#843: up --wait-timeout, and it requires --wait -------------------

#[test]
fn up_has_wait_timeout_that_requires_wait() {
	assert!(help_of("up").contains("--wait-timeout"));
	// Without --wait it is a usage error (clap `requires`).
	assert!(is_clap_usage_error(&run_offline(&[
		"start",
		"--wait-timeout",
		"30"
	])));
	assert!(is_clap_usage_error(&run_offline(&[
		"up",
		"--wait-timeout",
		"30"
	])));
	// With --wait it parses.
	assert!(!is_clap_usage_error(&run_offline(&[
		"up",
		"--wait",
		"--wait-timeout",
		"30"
	])));
}

// --- #841: conflicting recreate/build flags are rejected ---------------------

#[test]
fn conflicting_recreate_and_build_flags_are_rejected() {
	for args in [
		&["up", "--no-recreate", "--force-recreate"][..],
		&["up", "--build", "--no-build"][..],
		&["create", "--no-recreate", "--force-recreate"][..],
	] {
		assert!(
			is_clap_usage_error(&run_offline(args)),
			"`{args:?}` should be rejected as conflicting"
		);
	}
}

// --- #842: negative --timeout rejected in both spellings --------------------

#[test]
fn negative_timeout_is_rejected_consistently() {
	for args in [
		&["down", "--timeout=-5"][..],
		&["down", "-t", "-5"][..],
		&["stop", "-t", "-1"][..],
	] {
		assert!(
			is_clap_value_error(&run_offline(args)),
			"`{args:?}` should be a clear range error"
		);
	}
	// Zero and positive are fine.
	assert!(!is_clap_value_error(&run_offline(&["down", "--timeout=0"])));
}

// --- #844: create --pull / --no-deps ----------------------------------------

#[test]
fn create_exposes_pull_and_no_deps() {
	let h = help_of("create");
	assert!(h.contains("--no-deps"), "create missing --no-deps:\n{h}");
	assert!(h.contains("--pull"), "create missing --pull:\n{h}");
}

// --- #845/#847/#848/#849/#850/#851/#854: new per-command flags in help ------

#[test]
fn new_command_flags_appear_in_help() {
	assert!(help_of("ls").contains("--filter"));
	let push = help_of("push");
	assert!(push.contains("--quiet"));
	let build = help_of("build");
	assert!(build.contains("--push") && build.contains("--progress"));
	assert!(help_of("run").contains("--label"));
	let events = help_of("events");
	assert!(
		events.contains("--since") && events.contains("--until") && events.contains("--filter")
	);
	assert!(help_of("attach").contains("--index"));
	let logs = help_of("logs");
	assert!(logs.contains("--no-color") && logs.contains("--no-log-prefix"));
	let config = help_of("config");
	for flag in [
		"--volumes",
		"--images",
		"--profiles",
		"--hash",
		"--no-normalize",
	] {
		assert!(config.contains(flag), "config missing {flag}:\n{config}");
	}
}

// --- #855: --ansi help mentions the NO_COLOR override -----------------------

#[test]
fn ansi_help_text_clarifies_no_color_override() {
	let out = Command::new(bin()).arg("--help").output().unwrap();
	let stdout = String::from_utf8_lossy(&out.stdout);
	assert!(
		stdout.contains("overrides"),
		"--ansi help should clarify that `always` overrides NO_COLOR:\n{stdout}"
	);
}

// --- #857: help tolerates extra args, -h/--help, and -- ---------------------

#[test]
fn help_subcommand_is_tolerant() {
	for args in [
		&["help", "up", "down"][..],
		&["help", "-h"][..],
		&["help", "--help"][..],
		&["help", "--", "up"][..],
		&["help"][..],
	] {
		let out = Command::new(bin())
			.args(args)
			.output()
			.expect("run help variant");
		assert!(
			out.status.success(),
			"`{args:?}` should exit 0; got {:?}\n{}",
			out.status.code(),
			String::from_utf8_lossy(&out.stderr)
		);
	}
	// `help up down` shows up's help.
	let out = Command::new(bin())
		.args(["help", "up", "down"])
		.output()
		.unwrap();
	assert!(String::from_utf8_lossy(&out.stdout).contains("Create and start"));
}

// --- #858/#859: missing subcommand exits non-zero ---------------------------

#[test]
fn missing_subcommand_exits_non_zero() {
	let none = Command::new(bin())
		.output()
		.expect("run podup with no args");
	assert!(!none.status.success(), "no args must exit non-zero");
	let gen = Command::new(bin())
		.arg("generate")
		.output()
		.expect("run generate");
	assert!(
		!gen.status.success(),
		"generate with no subcommand must exit non-zero"
	);
}

// --- #860: --project-directory must exist -----------------------------------

#[test]
fn missing_project_directory_is_rejected() {
	let out = Command::new(bin())
		.args(["--project-directory", "/no/such/podup/dir", "config"])
		.output()
		.expect("run with bad project-directory");
	assert!(
		!out.status.success(),
		"missing project directory must error"
	);
	assert!(
		String::from_utf8_lossy(&out.stderr).contains("project-directory"),
		"error should name --project-directory:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}

// --- #861: deprecated --json conflicts with an explicit --format ------------

#[test]
fn events_json_conflicts_with_explicit_format() {
	assert!(is_clap_usage_error(&run_offline(&[
		"events", "--json", "--format", "json"
	])));
	// Each spelling alone still parses.
	assert!(!is_clap_usage_error(&run_offline(&["events", "--json"])));
	assert!(!is_clap_usage_error(&run_offline(&[
		"events", "--format", "json"
	])));
}

// --- #863/#862: positional service / dash output reach runtime --------------

#[test]
fn images_and_export_positionals_parse() {
	assert!(!is_clap_usage_error(&run_offline(&["images", "web"])));
	assert!(!is_clap_usage_error(&run_offline(&[
		"export", "web", "-o", "-"
	])));
}

// --- #750: DOCKER_HOST with a remote scheme is rejected (not ignored) -------

#[test]
fn docker_host_remote_scheme_is_rejected() {
	let out = Command::new(bin())
		.arg("ls")
		.env_remove("PODMAN_SOCKET")
		.env("DOCKER_HOST", "tcp://127.0.0.1:2375")
		.output()
		.expect("run ls with DOCKER_HOST");
	assert!(!out.status.success(), "remote DOCKER_HOST must error");
	assert!(
		String::from_utf8_lossy(&out.stderr).contains("remote Podman"),
		"DOCKER_HOST=tcp:// should be rejected as remote:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}

// --- #856: config list projections render the expected lines ----------------

#[test]
fn config_projections_render_lists() {
	let dir = std::env::temp_dir().join(format!("podup-cflags-{}", std::process::id()));
	fs::create_dir_all(&dir).unwrap();
	let compose = dir.join("compose.yaml");
	// `web` is always active; `admin` lives behind the `frontend` profile so the
	// profile-aware projections are exercised when that profile is activated.
	fs::write(
		&compose,
		"services:\n  web:\n    image: nginx:1.27\n  admin:\n    image: busybox:1.36\n    profiles: [frontend]\nvolumes:\n  data: {}\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let volumes = Command::new(bin())
		.args(["-f", c, "config", "--volumes"])
		.output()
		.unwrap();
	assert!(volumes.status.success());
	assert!(String::from_utf8_lossy(&volumes.stdout)
		.lines()
		.any(|l| l == "data"));

	let images = Command::new(bin())
		.args(["-f", c, "config", "--images"])
		.output()
		.unwrap();
	assert!(String::from_utf8_lossy(&images.stdout).contains("nginx:1.27"));

	// `--profiles` lists the declared profiles of the active services; activate
	// `frontend` so `admin` (and its profile) survive profile resolution.
	let profiles = Command::new(bin())
		.args(["-f", c, "--profile", "frontend", "config", "--profiles"])
		.output()
		.unwrap();
	assert!(String::from_utf8_lossy(&profiles.stdout)
		.lines()
		.any(|l| l == "frontend"));

	let hash = Command::new(bin())
		.args(["-f", c, "config", "--hash", "*"])
		.output()
		.unwrap();
	assert!(hash.status.success());
	assert!(String::from_utf8_lossy(&hash.stdout).contains("web "));

	let _ = fs::remove_dir_all(&dir);
}

/// `--sig-proxy` must accept docker's bare spelling.
///
/// It lives in the "accepted for compatibility" set, whose entire promise is
/// that a script written against docker compose runs unchanged. Docker declares
/// it as a bare boolean; podup declared it as one that demands a value, so
/// `docker compose attach --sig-proxy web` came back as a usage error and exit
/// 2 — a no-op flag rejecting the command it was supposed to tolerate.
///
/// Asserted as "not a clap error": these never reach Podman, so the run fails
/// later on the missing compose file, which is exactly the point — parsing got
/// past the flag.
#[test]
fn sig_proxy_accepts_dockers_bare_form_and_an_explicit_value() {
	for args in [
		vec!["attach", "web", "--sig-proxy"],
		vec!["attach", "web", "--sig-proxy=true"],
		vec!["attach", "web", "--sig-proxy=false"],
	] {
		let out = run_offline(&args);
		assert!(
			!is_clap_usage_error(&out) && !is_clap_value_error(&out),
			"{args:?} must parse; got {}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
}

/// Bare `podup` is the first screen anyone sees after installing, and it was
/// the one help path that rendered without colour — every other screen had it,
/// so podup looked like a tool that does not colourise at all.
///
/// This checks the piped side, which is what a test harness can observe: the
/// help must reach **stderr** (it is a usage error, not output), carry no
/// escape codes when stderr is not a terminal, and stay on exit 2. The coloured
/// side is verified by running it under a pty, which no `Command::output()`
/// test can do — 50 escapes against 0 before.
#[test]
fn bare_podup_prints_help_to_stderr_and_stays_plain_when_piped() {
	let out = Command::new(bin())
		.output()
		.expect("run podup with no args");
	let stderr = String::from_utf8_lossy(&out.stderr);
	let stdout = String::from_utf8_lossy(&out.stdout);

	assert_eq!(out.status.code(), Some(2), "no args is a usage error");
	assert!(
		stderr.contains("Usage:") && stderr.contains("Commands:"),
		"the help belongs on stderr: {stderr:?}"
	);
	assert!(
		stdout.is_empty(),
		"stdout must stay clean for scripts: {stdout:?}"
	);
	assert!(
		!stderr.contains('\u{1b}'),
		"a pipe must receive no escape codes: {stderr:?}"
	);
}

/// The same screen, whether or not the caller's environment happens to set one
/// of podup's env-backed globals.
///
/// `arg_required_else_help` only fires when there are NO arguments at all, and
/// an env-sourced one counts — so with `COMPOSE_PROJECT_NAME` or
/// `PODMAN_SOCKET` exported, which is the normal state of a real deployment,
/// bare `podup` printed a wall of forty-five subcommand names instead of its
/// help. This is how CI found it: the runner had one of them set and my first
/// version of the test above failed there while passing here.
#[test]
fn bare_podup_prints_the_same_help_with_env_globals_set() {
	for (key, value) in [
		("COMPOSE_PROJECT_NAME", "someproject"),
		("PODMAN_SOCKET", "/tmp/nonexistent.sock"),
		("COMPOSE_PROFILES", "dev"),
	] {
		let out = Command::new(bin())
			.env(key, value)
			.output()
			.unwrap_or_else(|e| panic!("run podup with {key} set: {e}"));
		let stderr = String::from_utf8_lossy(&out.stderr);
		assert_eq!(out.status.code(), Some(2), "with {key} set");
		assert!(
			stderr.contains("Commands:"),
			"{key} set must not replace the help with a subcommand list: {stderr:?}"
		);
	}
}

// --- Machine output never carries colour, even when forced ------------------

/// Machine-readable output must never carry colour, even when colour is
/// forced. A script parsing `--format json` or `-q` reads stdout, so any
/// escape landing there is data corruption, not decoration.
///
/// Forced with `--ansi always` on purpose: the auto-detection would suppress
/// colour in the test harness anyway, so a weaker version of this test would
/// pass without proving anything. Only stdout is checked: a connection
/// failure prints its `error:` banner to stderr, which is a human diagnostic
/// and is allowed to be coloured like any other podup error — the guarantee
/// under test is about the machine-parseable stream, not every byte podup
/// writes.
#[test]
fn machine_output_carries_no_escapes_even_with_colour_forced() {
	let dir = std::env::temp_dir().join(format!("podup-cflags-machine-{}", std::process::id()));
	fs::create_dir_all(&dir).unwrap();
	let compose = dir.join("compose.yaml");
	fs::write(&compose, "services:\n  web:\n    image: nginx:1.27\n").unwrap();
	let c = compose.to_str().unwrap();

	for args in [
		vec!["-f", c, "--ansi", "always", "config"],
		vec!["-f", c, "--ansi", "always", "ps", "--format", "json"],
		vec!["-f", c, "--ansi", "always", "ps", "-q"],
	] {
		let out = Command::new(bin())
			.args(&args)
			.output()
			.unwrap_or_else(|e| panic!("run podup {args:?}: {e}"));
		let stdout = String::from_utf8_lossy(&out.stdout);
		assert!(
			!stdout.contains('\u{1b}'),
			"{args:?} emitted an escape into machine output: {stdout:?}"
		);
	}

	let _ = fs::remove_dir_all(&dir);
}
