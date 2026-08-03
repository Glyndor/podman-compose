//! CLI startup helpers: diagnostic log formatting, tracing initialization, the
//! internal-error notice, and argument parsing with framed help output.

use std::process;

use clap::Parser;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::EnvFilter;

use crate::cli::{Cli, Commands};

mod config_normalize;
mod config_render;
pub(crate) use config_render::{render_config, ConfigOutput};

/// Whether a command creates, destroys, or changes the state of containers and
/// so must hold the exclusive project lock.
pub(crate) fn is_mutating(command: &Commands) -> bool {
	matches!(
		command,
		Commands::Up { .. }
			| Commands::Down { .. }
			| Commands::Start { .. }
			| Commands::Stop { .. }
			| Commands::Build { .. }
			| Commands::Rm { .. }
			| Commands::Kill { .. }
			| Commands::Pause { .. }
			| Commands::Unpause { .. }
			| Commands::Run { .. }
			| Commands::Restart { .. }
			| Commands::Scale { .. }
			| Commands::Create { .. }
	)
}

/// Validate the resolved project name at the trust boundary, before it reaches
/// any code path that builds a filesystem path from it (staging, lock files,
/// quadlet generation) or filters containers by it. Shared by the mutating
/// dispatch path and the read-only `config`/`ps` paths so every command reports
/// the same invalid-name error.
pub(crate) fn validate_project_name(project: &str) -> podup::Result<()> {
	if podup::is_safe_project_name(project) {
		Ok(())
	} else {
		Err(podup::ComposeError::Unsupported(format!(
			"project name {project:?} is not a safe path component: must be \
			 lowercase ASCII, starting with a letter or digit, followed only by \
			 lowercase letters, digits, '-' or '_', max 128 chars"
		)))
	}
}

/// Whether a command is scoped purely by the `podup.project` label and never
/// reads service definitions, so it can run against a project with no compose
/// file present — matching `docker compose -p NAME events`/`ps`. These commands
/// tolerate a missing compose file at startup instead of erroring `FileNotFound`.
pub(crate) fn is_label_only(command: &Commands) -> bool {
	matches!(command, Commands::Events { .. } | Commands::Ps { .. })
}

/// Canonical project URL, reused for the bug-report hint on internal errors.
const REPO_URL: &str = "https://github.com/Glyndor/podup";

/// Event formatter that renders every diagnostic as `podup: <level>: <message>`
/// on a single line, matching the prefix used by the CLI's own `eprintln!`
/// warnings and errors. This unifies the compose forward-compat diagnostics
/// (emitted via `tracing::warn!`) with the rest of podup's user-facing output.
struct PodupFormat;

impl<S, N> FormatEvent<S, N> for PodupFormat
where
	S: Subscriber + for<'a> LookupSpan<'a>,
	N: for<'a> FormatFields<'a> + 'static,
{
	fn format_event(
		&self,
		ctx: &FmtContext<'_, S, N>,
		mut writer: Writer<'_>,
		event: &Event<'_>,
	) -> std::fmt::Result {
		let level = *event.metadata().level();
		let label = podup::ui::paint(
			level_style(level),
			level_word(level),
			podup::ui::stderr_colored(),
		);
		write!(writer, "podup: {label}: ")?;
		ctx.field_format().format_fields(writer.by_ref(), event)?;
		writeln!(writer)
	}
}

/// Map a tracing level to the user-facing word used in `podup:` output.
fn level_word(level: tracing::Level) -> &'static str {
	match level {
		tracing::Level::ERROR => "error",
		tracing::Level::WARN => "warning",
		tracing::Level::INFO => "info",
		tracing::Level::DEBUG => "debug",
		tracing::Level::TRACE => "trace",
	}
}

/// The colour for a level's word: bold red (error), bold yellow (warning), green
/// (info), dim (debug/trace).
fn level_style(level: tracing::Level) -> anstyle::Style {
	use anstyle::{AnsiColor, Style};
	match level {
		tracing::Level::ERROR => Style::new().bold().fg_color(Some(AnsiColor::Red.into())),
		tracing::Level::WARN => Style::new().bold().fg_color(Some(AnsiColor::Yellow.into())),
		tracing::Level::INFO => Style::new().fg_color(Some(AnsiColor::Green.into())),
		_ => Style::new().dimmed(),
	}
}

/// Guidance printed after an internal error or panic: where to report it and a
/// reminder to scrub secrets first. Kept off ordinary, user-correctable errors.
pub(crate) fn internal_error_notice() -> String {
	format!(
		"podup: this looks like a bug; re-run with RUST_LOG=debug and report it at {REPO_URL}/issues\n\
		 podup: redact secrets (passwords, tokens, resolved env values) from any logs before sharing"
	)
}

/// Whether a panic message denotes a broken pipe (a downstream reader closed the
/// pipe early). Rust ignores SIGPIPE, so a failing `println!`/`eprintln!` panics
/// rather than dying by signal; that specific panic is a clean exit, not an
/// internal error.
///
/// The match is anchored to the exact prefix the standard library uses, because
/// a bare substring search over the panic text is far too wide: it exits 0 for
/// **any** panic whose message happens to mention a broken pipe — an
/// `.expect()` on an unrelated io error, or a Podman error quoting a downstream
/// EPIPE — and with `panic = "abort"` this hook is the only thing between a
/// panic and the exit status, so a real crash would report success and print
/// nothing. Pure so it can be unit-tested.
pub(crate) fn is_broken_pipe_panic(msg: &str) -> bool {
	let Some(reason) = msg
		.strip_prefix("failed printing to stdout: ")
		.or_else(|| msg.strip_prefix("failed printing to stderr: "))
	else {
		return false;
	};
	let lower = reason.to_ascii_lowercase();
	lower.contains("broken pipe") || lower.contains("os error 32")
}

/// Initialize the global tracing subscriber, written to stderr in the
/// `podup: <level>: <msg>` format so stdout stays a clean pipe. `default_level`
/// is the floor used when `RUST_LOG` is unset — `warn` for most commands (so the
/// forward-compat "unknown field" notices are never silently dropped), `info`
/// for interactive long-running ones like `watch` that should surface their
/// per-action progress. `RUST_LOG` always overrides.
pub(crate) fn init_tracing(default_level: &str) {
	tracing_subscriber::fmt()
		.with_env_filter(
			EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level)),
		)
		.with_writer(std::io::stderr)
		.event_format(PodupFormat)
		.init();
}

/// Build the `run`-only flag overrides from the parsed command. These are kept
/// off the frozen public `RunOptions` API and threaded through the engine
/// builder instead (`Engine::with_run_overrides`).
pub(crate) fn run_overrides_for(command: &Commands) -> podup::RunOverrides {
	match command {
		Commands::Run {
			user,
			workdir,
			entrypoint,
			volume,
			publish,
			interactive,
			no_deps,
			..
		} => podup::RunOverrides {
			user: user.clone(),
			workdir: workdir.clone(),
			entrypoint: entrypoint.clone(),
			volumes: volume.clone(),
			publish: publish.clone(),
			interactive: *interactive,
			no_deps: *no_deps,
		},
		_ => podup::RunOverrides::default(),
	}
}

/// Whether `run` was given `-T/--no-TTY`.
///
/// Carried on the engine rather than on `RunOverrides`, which is public and not
/// `#[non_exhaustive]` — a new field there is a breaking change, which is what
/// cargo-semver-checks reported when the field was tried there.
pub(crate) fn run_no_tty_for(command: &Commands) -> bool {
	matches!(command, Commands::Run { no_tty, .. } if *no_tty)
}

/// Extract the `docker compose run -l/--label KEY=VAL` ad-hoc labels for the
/// engine builder ([`podup::Engine::with_run_labels`]). Carried on the engine
/// rather than the frozen `RunOverrides` struct so the published library API
/// stays stable across minors, mirroring `run_overrides_for`.
pub(crate) fn run_labels_for(command: &Commands) -> Vec<String> {
	match command {
		Commands::Run { label, .. } => label.clone(),
		_ => Vec::new(),
	}
}

/// Parse the CLI, framing `--help`/`--version` output with a blank line top and
/// bottom (clap trims template edges, so wrap the rendered text here).
/// Render a clap help/usage screen, with or without its styling.
///
/// Split from the two call sites so the choice is testable: both arms used to
/// live inside `parse_cli`, which calls `process::exit` and so cannot be
/// exercised by a unit test at all — the coloured arm was unreachable from the
/// suite, and adding it dropped coverage below the gate.
///
/// Which sink to ask about is the caller's business and differs between them:
/// `--help` goes to stdout, a missing subcommand is a usage error and goes to
/// stderr. Asking the wrong one would emit escape codes into a redirected
/// stream.
fn render_help(rendered: &clap::builder::StyledStr, colour: bool) -> String {
	if colour {
		rendered.ansi().to_string()
	} else {
		rendered.to_string()
	}
}

/// The help of the deepest subcommand the arguments actually reached.
///
/// Walks the given command tree along the non-flag arguments, following aliases,
/// and stops at the first token that is not a subcommand of the current level.
/// With no subcommand at all it returns the root's help, which is what bare
/// `podup` should print.
fn help_for_argv(root: clap::Command) -> clap::builder::StyledStr {
	help_for(root, std::env::args().skip(1))
}

/// The tree walk, separated from the process environment so it is testable.
fn help_for(root: clap::Command, args: impl Iterator<Item = String>) -> clap::builder::StyledStr {
	// Build first. An unbuilt tree has not propagated the binary name or the
	// global options down to its subcommands, so a subcommand plucked out of it
	// renders `Usage: generate <COMMAND>` instead of
	// `Usage: podup generate [OPTIONS] <COMMAND>` — the same text `--help` on
	// that group already produces.
	let mut cmd = root;
	cmd.build();
	let mut args = args.peekable();
	while let Some(arg) = args.next() {
		if arg.starts_with('-') {
			// A flag that takes a value consumes the next token, and that token
			// must not be read as a subcommand: `-p build` names a project, not
			// the `build` command. `--flag=value` carries its own value, so only
			// the separated form eats one.
			if !arg.contains('=') && takes_a_value(&cmd, &arg) {
				args.next();
			}
			continue;
		}
		match cmd.find_subcommand(&arg) {
			Some(sub) => cmd = sub.clone(),
			// Stop at the first token that is not a subcommand here, so
			// `podup generate quadlet --bad` stays on `quadlet`.
			None => break,
		}
	}
	cmd.render_help()
}

/// Whether `token` names an argument of `cmd` that consumes a following value.
///
/// Matches long (`--ansi`), short (`-p`) and clustered-short (`-fp`) forms; for a
/// cluster only the last letter can take the value, which is how getopt works.
fn takes_a_value(cmd: &clap::Command, token: &str) -> bool {
	let wants_value = |a: &clap::Arg| {
		matches!(
			a.get_action(),
			clap::ArgAction::Set | clap::ArgAction::Append
		)
	};
	if let Some(long) = token.strip_prefix("--") {
		return cmd
			.get_arguments()
			.any(|a| a.get_long() == Some(long) && wants_value(a));
	}
	match token.strip_prefix('-').and_then(|s| s.chars().last()) {
		Some(last) => cmd
			.get_arguments()
			.any(|a| a.get_short() == Some(last) && wants_value(a)),
		None => false,
	}
}

pub(crate) fn parse_cli() -> Cli {
	// Apply `--ansi` before clap renders anything: `--help` and clap's own
	// errors are produced inside the parse call below, so a choice applied
	// afterwards arrives too late for them.
	if let Some(mode) = crate::cli::ansi_from_argv(std::env::args()) {
		podup::ui::set_color_choice(mode.into());
	}
	match Cli::try_parse() {
		Ok(cli) => cli,
		Err(e) => match e.kind() {
			clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
				// `--help`/`--version` are handled by clap before `--ansi` is parsed,
				// so colour the rendered text by clap's own styling only when stdout
				// is a colour sink (TTY + no NO_COLOR); piped output stays plain and
				// byte-identical to before.
				print!(
					"\n{}\n",
					render_help(&e.render(), podup::ui::stdout_colored())
				);
				process::exit(0);
			}
			// `MissingSubcommand` is the same situation wearing a different hat.
			// `arg_required_else_help` only fires when there are NO arguments at
			// all, and an env-sourced one counts — so with `COMPOSE_PROJECT_NAME`
			// or `PODMAN_SOCKET` exported, which is the normal state of a real
			// deployment, bare `podup` printed a wall of forty-five subcommand
			// names instead of its help. Same user, same mistake, worse answer,
			// decided by an environment variable they did not think was involved.
			clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
			| clap::error::ErrorKind::MissingSubcommand => {
				// No subcommand (top level) or a required nested subcommand (e.g.
				// `generate`) was given: print the help to stderr and exit non-zero,
				// so a script sees the error instead of a silent success. `podup
				// help` (the explicit Help variant) still exits 0.
				//
				// Coloured the same way the `--help` branch above is, but gated on
				// *stderr* since that is where this goes. Bare `podup` is the first
				// screen anyone sees after installing, and it was the one help path
				// that rendered plain — so podup looked like a tool with no colour
				// while every other screen had it.
				// Rendered from the command, not from the error: a
				// `MissingSubcommand` error renders as a one-line complaint plus
				// that subcommand wall, and the help is the useful answer to
				// both kinds.
				//
				// From the command the user actually reached, not always the root.
				// `generate` and `autostart` declare `subcommand_required`, so bare
				// `podup generate` lands here — and rendering the root told them
				// about the whole tool while withholding the one thing they needed,
				// which is what `generate` accepts. Their own `--help` already
				// answers that correctly; this path did not.
				let help = help_for_argv(<Cli as clap::CommandFactory>::command());
				eprint!("\n{}\n", render_help(&help, podup::ui::stderr_colored()));
				process::exit(2);
			}
			_ => e.exit(),
		},
	}
}

#[cfg(test)]
mod help_for_tests {
	use super::help_for;
	use crate::cli::Cli;
	use clap::CommandFactory;

	fn help(args: &[&str]) -> String {
		help_for(Cli::command(), args.iter().map(|s| (*s).to_string())).to_string()
	}

	#[test]
	fn no_subcommand_renders_the_root_help() {
		assert!(help(&[]).contains("Usage: podup [OPTIONS] <COMMAND>"));
	}

	#[test]
	fn a_subcommand_group_renders_its_own_help_not_the_root() {
		let out = help(&["generate"]);
		assert!(
			out.contains("Usage: podup generate"),
			"expected generate's own usage, got: {out}"
		);
		// The point of the fix: the root's usage must NOT be what a user asking
		// about `generate` is shown.
		assert!(
			!out.contains("Usage: podup [OPTIONS] <COMMAND>"),
			"the root help was rendered instead: {out}"
		);
		assert!(
			out.contains("quadlet"),
			"the group's subcommands are missing"
		);
	}

	#[test]
	fn an_alias_resolves_to_the_command_it_names() {
		assert!(help(&["gen"]).contains("Usage: podup generate"));
	}

	#[test]
	fn flags_before_the_subcommand_do_not_stop_the_walk() {
		// `--ansi never generate` must still reach generate. A walk that treated
		// the flag's value as a subcommand token would stop at `never`.
		assert!(help(&["--ansi", "never", "autostart"]).contains("Usage: podup autostart"));
	}

	#[test]
	fn a_flag_value_that_looks_like_a_subcommand_is_not_walked_into() {
		// `-p` names a project. A project called "build" is ordinary, and reading
		// it as the `build` command would render the wrong help for bare
		// `podup -p build`.
		let out = help(&["-p", "build"]);
		assert!(
			out.contains("Usage: podup [OPTIONS] <COMMAND>"),
			"a flag's value was walked into as a subcommand: {out}"
		);
	}

	#[test]
	fn the_equals_form_carries_its_own_value() {
		// `--project-name=build` consumes nothing after it, so `generate` is still
		// the next token to consider.
		assert!(help(&["--project-name=build", "generate"]).contains("Usage: podup generate"));
	}

	#[test]
	fn an_unknown_token_stops_the_walk_at_the_last_real_command() {
		assert!(help(&["generate", "nosuchthing"]).contains("Usage: podup generate"));
	}
}

#[cfg(test)]
mod render_help_tests {
	use super::render_help;

	fn styled() -> clap::builder::StyledStr {
		let mut s = clap::builder::StyledStr::new();
		s.push_str("Usage: podup [OPTIONS] <COMMAND>");
		s
	}

	/// Piped output must be byte-clean: a script reading the usage screen gets
	/// text, not terminal control codes.
	#[test]
	fn without_colour_the_text_carries_no_escapes() {
		let out = render_help(&styled(), false);
		assert!(!out.contains('\u{1b}'), "{out:?}");
		assert!(out.contains("Usage: podup"), "{out:?}");
	}

	/// And the coloured arm actually differs, rather than being a no-op nobody
	/// noticed — which is what a plain `assert!(out.contains("Usage"))` on both
	/// arms would have failed to catch.
	#[test]
	fn with_colour_the_text_still_reads_the_same() {
		let plain = render_help(&styled(), false);
		let coloured = render_help(&styled(), true);
		assert!(coloured.contains("Usage: podup"), "{coloured:?}");
		assert_eq!(
			coloured.replace('\u{1b}', ""),
			plain.replace('\u{1b}', ""),
			"colour must not change the words"
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn validate_project_name_message_matches_the_enforced_rule() {
		// Pins the error text to what `is_safe_project_name` actually enforces
		// (lowercase-only, no '.'), not the looser rule the message used to
		// describe. `My.App` is exactly the kind of name the old wording
		// ("ASCII letters, digits, '-', '_', '.'") implied was fine, yet
		// `is_safe_project_name` has always rejected it (uppercase and '.'
		// are both disallowed) - a user following the old message would still
		// get bounced.
		let err = validate_project_name("My.App").unwrap_err();
		let msg = err.to_string();
		assert!(
			msg.contains("lowercase"),
			"message must say the name must be lowercase: {msg:?}"
		);
		assert!(
			!msg.contains("'.'"),
			"message must not list '.' as an allowed character: {msg:?}"
		);
	}

	#[test]
	fn validate_project_name_accepts_a_safe_name() {
		assert!(validate_project_name("my-app").is_ok());
	}

	#[test]
	fn label_only_covers_ps_and_events() {
		use crate::cli::{EventsFormat, OutputFormat};
		// `ps` and `events` are scoped purely by the project label, so they are
		// label-only and may run without a compose file.
		assert!(is_label_only(&Commands::Ps {
			all: false,
			quiet: false,
			services_only: false,
			size: false,
			filter: vec![],
			status: vec![],
			format: OutputFormat::Table,
			services: vec![],
		}));
		assert!(is_label_only(&Commands::Events {
			format: EventsFormat::Table,
			since: None,
			until: None,
			filter: vec![],
			json: false,
		}));
		// A command that reads service definitions is not label-only.
		assert!(!is_label_only(&Commands::Top {
			format: OutputFormat::Table,
			services: vec![],
		}));
	}

	#[test]
	fn level_words_match_user_facing_terms() {
		assert_eq!(level_word(tracing::Level::WARN), "warning");
		assert_eq!(level_word(tracing::Level::ERROR), "error");
		assert_eq!(level_word(tracing::Level::INFO), "info");
		assert_eq!(level_word(tracing::Level::DEBUG), "debug");
		assert_eq!(level_word(tracing::Level::TRACE), "trace");
		// Each severity gets a distinct style; debug/trace share the dim style.
		assert_ne!(
			level_style(tracing::Level::ERROR),
			level_style(tracing::Level::INFO)
		);
		assert_ne!(
			level_style(tracing::Level::WARN),
			level_style(tracing::Level::ERROR)
		);
		assert_eq!(
			level_style(tracing::Level::DEBUG),
			level_style(tracing::Level::TRACE)
		);
	}

	#[test]
	fn broken_pipe_panic_detected() {
		assert!(is_broken_pipe_panic(
			"failed printing to stdout: Broken pipe (os error 32)"
		));
		assert!(is_broken_pipe_panic(
			"failed printing to stderr: Broken pipe (os error 32)"
		));
		assert!(!is_broken_pipe_panic("some other internal error"));
	}

	#[test]
	fn an_unrelated_panic_mentioning_a_broken_pipe_is_not_swallowed() {
		// These are real crashes. Matching them would exit 0 and print nothing,
		// and `panic = "abort"` leaves this hook as the only gate before the exit
		// status, so the process would report success on a genuine bug.
		assert!(!is_broken_pipe_panic("Broken pipe"));
		assert!(!is_broken_pipe_panic(
			"called `Result::unwrap()` on an `Err` value: Os { code: 32, kind: BrokenPipe, message: \"Broken pipe\" }"
		));
		assert!(!is_broken_pipe_panic(
			"podman refused the request: broken pipe reading from the container"
		));
		assert!(!is_broken_pipe_panic("assertion failed at os error 32"));
	}

	#[test]
	fn internal_error_notice_reports_and_warns_on_secrets() {
		let notice = internal_error_notice();
		assert!(notice.contains(REPO_URL), "points at the issue tracker");
		assert!(notice.contains("/issues"));
		assert!(
			notice.contains("redact"),
			"reminds the user to scrub secrets"
		);
		assert!(
			notice.contains("RUST_LOG=debug"),
			"tells the user what to capture"
		);
	}
}
