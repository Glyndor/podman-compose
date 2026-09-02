//! Command-line interface definitions for `podup`.

use std::path::PathBuf;

use clap::Parser;

mod commands;
mod parse;
mod types;
pub(crate) use commands::Commands;
pub(crate) use types::{
	AnsiMode, AutostartCommands, AutostartMode, ConfigFormat, EventsFormat, GenerateCommands,
	OutputFormat, RmiScope,
};

/// Help-screen colours.
///
/// Deliberately avoids green and cyan. Green is the "healthy" colour everywhere
/// else in podup and cyan is an identity colour, so using them here put the two
/// most-read surfaces in competition for one vocabulary. Help uses weight
/// (bold, underline, dim) and no explicit foreground colour, which means
/// nothing elsewhere.
///
/// No slot sets an explicit `AnsiColor`, `error`/`invalid` excepted — those are
/// status colours (red), not identity, and stay. An explicit `White` was tried
/// first, on the reasoning that *some* colour should mark headings; measured
/// against the same reference palette this branch's tests pin
/// (`ui::palette_tests`), it read 1.82:1 on a white terminal background — worse
/// than the green/cyan it replaced — and bold commonly promotes `White` to the
/// terminal's bright-white ANSI code, which measured 1.00:1 on white, i.e.
/// invisible. Leaving the foreground unset uses the terminal's own default
/// text colour instead, which is readable on both themes by construction: it
/// is what the terminal owner already chose for their body text.
///
/// All eight slots are still explicitly set, even where that means an empty
/// style — clap's own `plain()` already leaves `error:` unstyled while podup
/// printed it bold red, so nothing here may silently fall back to a starting
/// point that disagreed with the rest of the binary.
const HELP_STYLES: clap::builder::Styles = clap::builder::Styles::plain()
	.header(clap::builder::styling::Style::new().bold().underline())
	.usage(clap::builder::styling::Style::new().bold())
	.literal(clap::builder::styling::Style::new().bold())
	.placeholder(clap::builder::styling::Style::new().dimmed())
	.error(clap::builder::styling::AnsiColor::Red.on_default().bold())
	.valid(clap::builder::styling::Style::new().bold())
	.invalid(clap::builder::styling::AnsiColor::Red.on_default())
	.context(clap::builder::styling::Style::new());

/// Read `--ansi` straight off argv, before clap parses anything.
///
/// clap renders `--help` inside the parse call, and the colour choice was only
/// applied after it returned — so `podup --ansi never --help` came out coloured
/// while `NO_COLOR=1 podup --help` did not. One flag, two answers.
///
/// Accepts both spellings clap does (`--ansi never`, `--ansi=never`) and is
/// deliberately forgiving: an unrecognised value yields `None` and leaves the
/// real parser to produce the error message.
pub(crate) fn ansi_from_argv<I: Iterator<Item = String>>(args: I) -> Option<AnsiMode> {
	let mut args = args.peekable();
	while let Some(arg) = args.next() {
		// `--` ends podup's own options: `run`/`exec`/`help` forward everything
		// after it to the container's command with `allow_hyphen_values`, so a
		// passthrough argument may legitimately read `--ansi always` and must not
		// be mistaken for podup's flag. clap already stops here; before this, the
		// pre-scan did not, and `NO_COLOR=1 podup <typo> exec svc -- --ansi always`
		// painted clap's error in colour.
		if arg == "--" {
			return None;
		}
		let value = if let Some(v) = arg.strip_prefix("--ansi=") {
			v.to_string()
		} else if arg == "--ansi" {
			args.next()?
		} else {
			continue;
		};
		return match value.as_str() {
			"auto" => Some(AnsiMode::Auto),
			"always" => Some(AnsiMode::Always),
			"never" => Some(AnsiMode::Never),
			_ => None,
		};
	}
	None
}

/// Top-level clap parser for the `podup` CLI; fields carry the per-flag docs.
//
// An explicit `about` is set on `#[command]` so clap does not promote this
// internal doc comment to the program's `--help` description.
#[derive(Parser)]
#[command(
	name = "podup",
	// clap renders `--version` as `{name} {version}`, so the derived default gave
	// `podup 3.3.0` while the `version` subcommand gave `podup version v3.3.0` —
	// two answers to the same question, and neither caller can tell which one it
	// is going to get. Measured on docker-compose v5.1.3: `version` and
	// `--version` are byte-identical (`Docker Compose version v5.1.3`) and only
	// `--short` drops the `v`. Folding the word and the prefix into the version
	// string is what makes clap emit the same line the subcommand does; the
	// subcommand still owns `--short` and `--format json`.
	version = concat!("version v", env!("CARGO_PKG_VERSION")),
	about = "Run Compose projects on Podman.",
	styles = HELP_STYLES,
	// No subcommand prints help and exits non-zero (like docker compose), and the
	// built-in `help` is replaced by an explicit `Help` variant that tolerates
	// extra tokens, `-h`/`--help`, and a leading `--`.
	arg_required_else_help = true,
	disable_help_subcommand = true
)]
pub(crate) struct Cli {
	/// Path to the compose file (or `COMPOSE_FILE`). Unset: probe the
	/// compose-spec precedence list (compose.yaml/.yml, docker-compose.yaml/.yml).
	// Not `global`: its `-f` short would collide with subcommand `-f` flags
	// (e.g. `rm --force`), which clap forbids. Must precede the subcommand.
	#[arg(short, long)]
	pub(crate) file: Vec<PathBuf>,

	/// Project name, the container-name prefix (or `COMPOSE_PROJECT_NAME`).
	/// Unset: the top-level `name:`, then the sanitized project-directory basename.
	// Not `global`: its `-p` short would collide with subcommand `-p` flags
	// (e.g. `run --publish`). Must precede the subcommand. `--project-name` is
	// docker compose's long form; scripts written for it must work verbatim.
	#[arg(
		short,
		long,
		visible_alias = "project-name",
		env = "COMPOSE_PROJECT_NAME"
	)]
	pub(crate) project: Option<String>,

	/// Podman socket path (overrides auto-detection and PODMAN_SOCKET env).
	/// `global` so it can appear before or after the subcommand (it has no
	/// short flag, so there is no collision).
	#[arg(long, env = "PODMAN_SOCKET", global = true)]
	pub(crate) socket: Option<String>,

	/// Maximum number of HTTP/1.1 connections the libpod client keeps open
	/// to the Podman socket for reuse (or `PODUP_LIBCOD_POOL`). Buffered calls
	/// share the pool; streaming calls each take a dedicated connection
	/// outside this cap. Default: 8.
	#[arg(long, env = "PODUP_LIBCOD_POOL", global = true, value_name = "N")]
	pub(crate) connection_pool_size: Option<usize>,

	/// Active profiles (comma-separated).  May also be set via `COMPOSE_PROFILES`.
	#[arg(long, value_delimiter = ',', env = "COMPOSE_PROFILES", global = true)]
	pub(crate) profile: Vec<String>,

	/// Base directory for relative paths (env_file, build context, bind mounts,
	/// config/secret sources). Defaults to the compose file's directory.
	#[arg(long, global = true)]
	pub(crate) project_directory: Option<PathBuf>,

	/// Extra env file(s) for interpolation (repeatable; later `--env-file` wins, and these replace the default `.env`; process env still wins).
	/// With `run`, they also seed the one-off container's environment (below `environment:`/`-e`).
	#[arg(long = "env-file", global = true)]
	pub(crate) env_file: Vec<String>,

	/// When to colourise output: auto (TTY only), always, or never. With `auto`,
	/// `NO_COLOR` also forces plain output; `--ansi always` overrides `NO_COLOR`.
	#[arg(long, value_enum, default_value_t = AnsiMode::Auto, global = true)]
	pub(crate) ansi: AnsiMode,

	/// Suppress the host-binding / privilege-escalation warnings the engine
	/// emits during `up`/`create`/`run`/`exec` (e.g. `network_mode: host`,
	/// `privileged: true`, `pid: host`, `container:<id>` namespace sharing).
	/// Operators who wrote the compose file deliberately use this to silence
	/// the per-run warning. `podup config` still surfaces the active modes —
	/// that command is the "show me what will happen" path, where the
	/// warning is the whole point.
	// `global` so it works on every subcommand that may build a container spec
	// (`up`, `create`, `run`, `exec`). The subcommand flags use `-q` for their
	// own `--quiet`, so the long form is the only spelling here.
	#[arg(long, global = true)]
	pub(crate) no_warn: bool,

	#[command(subcommand)]
	pub(crate) command: Commands,
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
