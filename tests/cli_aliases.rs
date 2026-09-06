//! CLI surface checks for the convenience command aliases and the blank-line
//! framing around `--help`/`--version` output. Aliases are hidden (they must
//! resolve when typed but stay out of the top-level help listing), and the
//! help/version text is wrapped with one blank line top and bottom.

use std::process::Command;

fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

/// Every hidden alias must resolve to its canonical command: invoking
/// `<alias> --help` exits 0 and prints that command's help (clap rejects an
/// unknown subcommand with a non-zero exit instead).
#[test]
fn aliases_resolve_to_their_command() {
	let cases = [
		("convert", "resolved compose file"),
		("gen", "declarative artifacts"),
		("resume", "Resume paused"),
		("remove", "Remove stopped"),
		("log", "output from containers"),
		("image", "images used by services"),
	];
	for (alias, needle) in cases {
		let output = Command::new(bin())
			.args([alias, "--help"])
			.output()
			.expect("run alias --help");
		assert!(
			output.status.success(),
			"alias `{alias}` did not resolve (exit {:?})",
			output.status.code()
		);
		let stdout = String::from_utf8_lossy(&output.stdout);
		assert!(
			stdout.contains(needle),
			"alias `{alias}` help missing {needle:?}; got:\n{stdout}"
		);
	}
}

/// Aliases are hidden: the top-level `--help` lists canonical commands only,
/// never the alias spellings.
#[test]
fn aliases_stay_out_of_top_level_help() {
	let output = Command::new(bin())
		.arg("--help")
		.output()
		.expect("run --help");
	let stdout = String::from_utf8_lossy(&output.stdout);
	for alias in ["convert", "resume", "remove"] {
		assert!(
			!stdout.lines().any(|l| l.trim_start().starts_with(alias)),
			"alias `{alias}` leaked into top-level help"
		);
	}
}

/// `--help` and `--version` are framed with exactly one blank line at the top
/// and bottom.
#[test]
fn help_and_version_are_blank_line_framed() {
	for flag in ["--help", "--version"] {
		let output = Command::new(bin()).arg(flag).output().expect("run flag");
		assert!(output.status.success());
		let stdout = String::from_utf8_lossy(&output.stdout);
		assert!(stdout.starts_with('\n'), "{flag}: no leading blank line");
		assert!(stdout.ends_with("\n\n"), "{flag}: no trailing blank line");
		assert!(
			!stdout.starts_with("\n\n"),
			"{flag}: more than one leading blank line"
		);
	}
}

/// `-t/--timeout` (shutdown grace) is accepted by every command that stops
/// containers (up, down, stop, restart) matching docker compose.
#[test]
fn timeout_flag_is_accepted_by_stop_commands() {
	for cmd in ["up", "down", "stop", "restart"] {
		let output = Command::new(bin())
			.args([cmd, "--help"])
			.output()
			.expect("run <cmd> --help");
		assert!(output.status.success(), "`{cmd} --help` failed");
		let stdout = String::from_utf8_lossy(&output.stdout);
		assert!(
			stdout.contains("--timeout"),
			"`{cmd}` is missing the --timeout flag; got:\n{stdout}"
		);
	}
}

/// `exec` accepts the docker-compose-style overrides (`-e/-u/-w/--privileged/
/// --index`).
#[test]
fn exec_accepts_override_flags() {
	let output = Command::new(bin())
		.args(["exec", "--help"])
		.output()
		.expect("run exec --help");
	assert!(output.status.success(), "`exec --help` failed");
	let stdout = String::from_utf8_lossy(&output.stdout);
	for flag in ["--env", "--user", "--workdir", "--privileged", "--index"] {
		assert!(
			stdout.contains(flag),
			"`exec` is missing the {flag} flag; got:\n{stdout}"
		);
	}
}

/// `build` accepts the docker-compose build overrides (`--no-cache`, `--pull`,
/// `--build-arg`, `-q/--quiet`).
#[test]
fn build_accepts_override_flags() {
	let output = Command::new(bin())
		.args(["build", "--help"])
		.output()
		.expect("run build --help");
	assert!(output.status.success(), "`build --help` failed");
	let stdout = String::from_utf8_lossy(&output.stdout);
	for flag in ["--no-cache", "--pull", "--build-arg", "--quiet"] {
		assert!(
			stdout.contains(flag),
			"`build` is missing the {flag} flag; got:\n{stdout}"
		);
	}
}

/// True when clap rejected the arguments itself (a usage error: exit code 2 with
/// the `error:`/`Usage:` banner) rather than the command parsing and then
/// failing later at runtime. Used to prove a flag combination *parses*.
fn is_clap_usage_error(out: &std::process::Output) -> bool {
	let stderr = String::from_utf8_lossy(&out.stderr);
	out.status.code() == Some(2) && stderr.contains("error:") && stderr.contains("Usage:")
}

/// `run --no-rm` parses and is mutually coherent with `--rm` (the two override
/// each other), so a one-off container can be kept after it exits. The pair is
/// also visible in `run --help`, while the default still removes.
#[test]
fn run_no_rm_parses_and_is_documented() {
	let help = Command::new(bin())
		.args(["run", "--help"])
		.output()
		.expect("run run --help");
	assert!(help.status.success());
	let help_out = String::from_utf8_lossy(&help.stdout);
	for flag in ["--rm", "--no-rm"] {
		assert!(
			help_out.contains(flag),
			"`run` is missing the {flag} flag; got:\n{help_out}"
		);
	}

	// `run web --no-rm` parses cleanly: it reaches runtime (and fails there,
	// connecting to a bogus socket) instead of being rejected by clap.
	let out = Command::new(bin())
		.args([
			"--socket",
			"/nonexistent.sock",
			"run",
			"web",
			"--no-rm",
			"true",
		])
		.output()
		.expect("run run --no-rm");
	assert!(
		!is_clap_usage_error(&out),
		"`run --no-rm` should parse; got clap usage error:\n{}",
		String::from_utf8_lossy(&out.stderr)
	);
}

/// Both spellings of the no-TTY flag parse on both `run` and `exec`, and both
/// are listed in each command's help.
///
/// docker-compose is inconsistent with itself here: `docker compose run` spells
/// the long form `--no-TTY` while `docker compose exec` spells it `--no-tty`.
/// A script copied from either command has to work, so podup accepts both on
/// both: a superset of docker rather than a guess at which one is canonical.
#[test]
fn no_tty_accepts_both_spellings_on_run_and_exec() {
	for cmd in ["run", "exec"] {
		let help = Command::new(bin())
			.args([cmd, "--help"])
			.output()
			.expect("run --help");
		assert!(help.status.success(), "`{cmd} --help` failed");
		let help_out = String::from_utf8_lossy(&help.stdout);
		for flag in ["--no-TTY", "--no-tty"] {
			assert!(
				help_out.contains(flag),
				"`{cmd}` help is missing {flag}; got:\n{help_out}"
			);
		}

		for flag in ["--no-TTY", "--no-tty", "-T"] {
			let out = Command::new(bin())
				.args(["--socket", "/nonexistent.sock", cmd, flag, "web", "true"])
				.output()
				.expect("run no-tty flag");
			assert!(
				!is_clap_usage_error(&out),
				"`{cmd} {flag}` should parse; got clap usage error:\n{}",
				String::from_utf8_lossy(&out.stderr)
			);
		}
	}
}

/// `logs` and `restart` accept a trailing multi-service list (like every sibling
/// command), so `logs a b` and `restart a b` parse rather than erroring on the
/// extra positional.
#[test]
fn logs_and_restart_accept_multiple_services() {
	for cmd in ["logs", "restart"] {
		let out = Command::new(bin())
			.args(["--socket", "/nonexistent.sock", cmd, "alpha", "beta"])
			.output()
			.unwrap_or_else(|e| panic!("run {cmd} alpha beta: {e}"));
		assert!(
			!is_clap_usage_error(&out),
			"`{cmd} alpha beta` should parse multiple services; got clap usage error:\n{}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
}

/// `events` exposes `--format <table|json>` (with possible-values in help) and
/// keeps the legacy `--json` as a hidden, still-accepted alias. Both spellings
/// parse.
#[test]
fn events_format_and_hidden_json_alias() {
	let help = Command::new(bin())
		.args(["events", "--help"])
		.output()
		.expect("run events --help");
	assert!(help.status.success());
	let help_out = String::from_utf8_lossy(&help.stdout);
	assert!(
		help_out.contains("--format"),
		"`events` is missing --format; got:\n{help_out}"
	);
	assert!(
		help_out.contains("table") && help_out.contains("json"),
		"`events --format` should list its possible values; got:\n{help_out}"
	);
	// The deprecated alias is hidden from help.
	assert!(
		!help_out.contains("--json"),
		"`events --json` must stay hidden; got:\n{help_out}"
	);

	// Both `--format json` and the hidden `--json` parse (reaching runtime).
	for args in [["events", "--format", "json"], ["events", "--json", ""]] {
		let argv: Vec<&str> = std::iter::once("--socket")
			.chain(std::iter::once("/nonexistent.sock"))
			.chain(args.iter().copied().filter(|a| !a.is_empty()))
			.collect();
		let out = Command::new(bin())
			.args(&argv)
			.output()
			.expect("run events variant");
		assert!(
			!is_clap_usage_error(&out),
			"`{argv:?}` should parse; got clap usage error:\n{}",
			String::from_utf8_lossy(&out.stderr)
		);
	}
}

/// `ps` and `images` expose output flags (`--format`, `-q/--quiet`; `ps` also
/// `-a/--all`) so their output can be scripted against.
#[test]
fn ps_and_images_expose_output_flags() {
	let ps = Command::new(bin()).args(["ps", "--help"]).output().unwrap();
	let ps_out = String::from_utf8_lossy(&ps.stdout);
	for flag in ["--all", "--quiet", "--format"] {
		assert!(ps_out.contains(flag), "`ps` missing {flag}:\n{ps_out}");
	}
	let img = Command::new(bin())
		.args(["images", "--help"])
		.output()
		.unwrap();
	let img_out = String::from_utf8_lossy(&img.stdout);
	for flag in ["--quiet", "--format"] {
		assert!(
			img_out.contains(flag),
			"`images` missing {flag}:\n{img_out}"
		);
	}
}
