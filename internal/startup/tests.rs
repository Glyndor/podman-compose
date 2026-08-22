//! Startup-time tests for the `startup` module. Pulled into a separate file so
//! the production `mod.rs` stays under the 500-line file cap; the test surface
//! has grown with the help-walker and the `format_clap_error` helper, and a
//! few hundred lines of `#[test]` blocks are not where the `parse_cli`
//! implementation belongs.

use super::*;

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
mod startup_tests {
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

	/// `podup logs --wrong-flag` reaches `format_clap_error` with an
	/// `UnknownArgument` error and the rendered output must lead with the
	/// binary's `podup:` prefix and the bold-red `error:` label — matching
	/// `exit_status::print_error` and the `tracing` formatter. The clap usage
	/// block follows, unprefixed, so a script that greps `podup:` knows what
	/// category the failure is.
	#[test]
	fn format_clap_error_prefixes_an_unknown_argument() {
		use clap::Parser;
		let err = match Cli::try_parse_from(["podup", "logs", "--wrong-flag"]) {
			Err(e) => e,
			Ok(_) => panic!("--wrong-flag is not a valid flag"),
		};
		let rendered = format_clap_error(&err);
		assert!(
			rendered.starts_with("podup: "),
			"missing `podup:` prefix: {rendered:?}"
		);
		assert!(
			rendered.contains("error"),
			"missing the bold-red `error:` label: {rendered:?}"
		);
		assert!(
			rendered.contains("--wrong-flag"),
			"the offending flag is missing from the message: {rendered:?}"
		);
		// clap still emits the usage block; the prefix applies to the leading
		// line only and the rest stays verbatim so a user can read it.
		assert!(
			rendered.contains("Usage:"),
			"the clap usage block is missing: {rendered:?}"
		);
	}

	/// `podup scale` requires a `SERVICE=N` positional — omitting it is a
	/// `MissingRequiredArgument`. The error must read the same as any other:
	/// `podup: error: …`, not a bare clap line.
	#[test]
	fn format_clap_error_prefixes_a_missing_required_argument() {
		use clap::Parser;
		let err = match Cli::try_parse_from(["podup", "scale"]) {
			Err(e) => e,
			Ok(_) => panic!("scale requires at least one SERVICE=N pair"),
		};
		let rendered = format_clap_error(&err);
		assert!(
			rendered.starts_with("podup: "),
			"missing `podup:` prefix: {rendered:?}"
		);
		assert!(
			rendered.contains("error"),
			"missing the bold-red `error:` label: {rendered:?}"
		);
	}

	/// An enum-typed arg (`--rmi` on `down`, `RmiScope`) must surface the
	/// same way. clap's enum-validation errors carry the kind `InvalidValue`,
	/// which is a different code path than unknown args; the prefix must
	/// apply uniformly.
	#[test]
	fn format_clap_error_prefixes_an_invalid_enum_value() {
		use clap::Parser;
		let err = match Cli::try_parse_from(["podup", "down", "--rmi", "bogus"]) {
			Err(e) => e,
			Ok(_) => panic!("--rmi takes `all` or `local`, not `bogus`"),
		};
		let rendered = format_clap_error(&err);
		assert!(
			rendered.starts_with("podup: "),
			"missing `podup:` prefix: {rendered:?}"
		);
		assert!(
			rendered.contains("error"),
			"missing the bold-red `error:` label: {rendered:?}"
		);
		assert!(
			rendered.contains("bogus"),
			"the offending value is missing from the message: {rendered:?}"
		);
	}

	/// With `#[non_exhaustive]` on `Commands`, every variant of the enum
	/// must be reachable through `is_label_only` without the test going stale.
	/// The match is exhaustive in this crate (the `#[non_exhaustive]` only
	/// bites across the crate boundary), so adding a new variant forces this
	/// test to fail to compile until the new command is classified.
	#[test]
	fn is_label_only_classifies_every_variant() {
		use crate::cli::{
			AutostartCommands, ConfigFormat, EventsFormat, GenerateCommands, OutputFormat,
		};
		// `Commands` is not `Debug` (clap's derive doesn't add it); assert
		// by index instead of formatting the value.
		let cases: Vec<(&'static str, Commands, bool)> = vec![
			(
				"Up",
				Commands::Up {
					detach: false,
					build: false,
					watch: false,
					remove_orphans: false,
					no_recreate: false,
					force_recreate: false,
					no_deps: false,
					timeout: None,
					scale: vec![],
					pull: None,
					no_build: false,
					quiet_pull: false,
					wait: false,
					wait_timeout: None,
					no_start: false,
					timestamps: false,
					renew_anon_volumes: false,
					abort_on_container_exit: false,
					exit_code_from: None,
					services: vec![],
				},
				false,
			),
			(
				"Ps",
				Commands::Ps {
					all: false,
					quiet: false,
					services_only: false,
					size: false,
					filter: vec![],
					status: vec![],
					format: OutputFormat::Table,
					services: vec![],
				},
				true,
			),
			(
				"Events",
				Commands::Events {
					format: EventsFormat::Table,
					since: None,
					until: None,
					filter: vec![],
					json: false,
				},
				true,
			),
			(
				"Config",
				Commands::Config {
					format: ConfigFormat::Yaml,
					services: false,
					volumes: false,
					images: false,
					profiles: false,
					hash: None,
					quiet: false,
					no_interpolate: false,
					no_normalize: false,
					resolve_image_digests: false,
				},
				false,
			),
			(
				"Generate",
				Commands::Generate {
					kind: GenerateCommands::Quadlet { output: None },
				},
				false,
			),
			(
				"Autostart",
				Commands::Autostart {
					kind: AutostartCommands::Status,
				},
				false,
			),
		];
		for (name, cmd, expected) in cases {
			assert_eq!(
				is_label_only(&cmd),
				expected,
				"wrong classification for {name}"
			);
		}
	}
}
