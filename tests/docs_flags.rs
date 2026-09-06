//! `docs/commands.md` must document every flag the CLI actually accepts.
//!
//! The command reference was written per command by hand and drifted every time
//! a flag landed: #1084 counted fourteen commands with missing flags and one
//! precedence rule that was simply false. Fixing the text once only resets the
//! clock; this test is what stops it recurring, by diffing `--help` against the
//! document rather than trusting either.
//!
//! It asserts one direction only: every flag clap exposes appears in the docs.
//! The reverse (documented flags that no longer exist) is a different failure
//! and a different fix, and folding both into one assertion would make the
//! message ambiguous.
//!
//! The match is scoped to the command's own `###` section. Searching the whole
//! document (which this did until #1132) means a flag documented anywhere
//! satisfies every command, so eight flags sat undocumented in their own tables
//! while the test stayed green. Scoping it is also the honest reading of what
//! the test claims to check: that a reader looking up `create` finds `--pull`
//! under `create`, not somewhere else entirely.
//!
//! It still checks names, never descriptions. A row can say the opposite of
//! what the code does and this will not notice: `exec`'s pseudo-TTY shipped
//! next to "podup never allocates one" (#1079), and `commit --pause` documented
//! a default of off while the code defaults it on (#1132).

use std::collections::BTreeSet;
use std::process::Command;

fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

/// Long flags clap prints in a `--help` body, e.g. `--no-cache`.
///
/// Only long forms are collected: a short form is always printed beside its long
/// one, so requiring the long spelling covers both without needing to model
/// clap's `-x, --xyz` layout.
fn flags_in_help(help: &str) -> BTreeSet<String> {
	let mut out = BTreeSet::new();
	for line in help.lines() {
		let trimmed = line.trim_start();
		// Flag lines start with the short form or the long one; anything else is
		// prose, a usage line, or a subcommand listing.
		if !trimmed.starts_with('-') {
			continue;
		}
		for token in trimmed.split_whitespace() {
			let token = token.trim_end_matches([',', '.']);
			if let Some(name) = token.strip_prefix("--") {
				let name = name
					.split(['=', '<', '['])
					.next()
					.unwrap_or_default()
					.trim_end_matches('>');
				if !name.is_empty() && name != "help" && name != "version" {
					out.insert(format!("--{name}"));
				}
			}
		}
	}
	out
}

/// The subcommands whose flag tables the reference documents. Aliases are
/// excluded: they resolve to the same command and share its table.
const COMMANDS: [&str; 29] = [
	"up", "down", "create", "start", "stop", "restart", "build", "ps", "ls", "logs", "events",
	"top", "stats", "port", "images", "volumes", "run", "exec", "cp", "attach", "kill", "rm",
	"wait", "scale", "commit", "export", "pull", "push", "config",
];

/// The lines of `docs/commands.md` documenting one command: from its `###`
/// heading to the next heading of any level.
///
/// Headings carry their positional arguments (``### `cp <SRC> <DST>` ``) so a
/// command matches when the first backticked word of the heading is its name.
/// `pause` and `unpause` share one heading, which the `/`-separated form covers.
fn section_for(docs: &str, cmd: &str) -> Option<String> {
	let mut lines = docs.lines();
	let heading = lines.by_ref().find(|line| {
		line.strip_prefix("### ").is_some_and(|rest| {
			rest.split('/').any(|part| {
				part.trim().trim_start_matches('`').split(['`', ' ']).next() == Some(cmd)
			})
		})
	});
	heading?;
	Some(
		lines
			// Both levels, and not a bare `#` test: a fenced bash block starts its
			// comments with `#` at column zero, and stopping there would truncate
			// the section early and fail a documented flag.
			.take_while(|line| !line.starts_with("## ") && !line.starts_with("### "))
			.collect::<Vec<_>>()
			.join("\n"),
	)
}

#[test]
fn every_cli_flag_is_documented() {
	let docs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/commands.md"))
		.expect("read docs/commands.md");

	// clap marks the compose-wide options `global`, so every command's `--help`
	// reprints them, but they are documented once, in the global table, not in
	// each command's own. Subtract them or the per-command check demands that
	// `--socket` be listed under all twenty-nine.
	let global = Command::new(bin())
		.arg("--help")
		.output()
		.expect("run --help");
	let globals = flags_in_help(&String::from_utf8_lossy(&global.stdout));

	let mut missing: Vec<String> = Vec::new();
	for cmd in COMMANDS {
		let out = Command::new(bin())
			.args([cmd, "--help"])
			.output()
			.unwrap_or_else(|e| panic!("run {cmd} --help: {e}"));
		assert!(out.status.success(), "`{cmd} --help` failed");
		let help = String::from_utf8_lossy(&out.stdout);
		let section = section_for(&docs, cmd)
			.unwrap_or_else(|| panic!("docs/commands.md has no `### {cmd}` section"));
		for flag in flags_in_help(&help) {
			if globals.contains(&flag) {
				continue;
			}
			if !section.contains(&flag) {
				missing.push(format!("{cmd}: {flag}"));
			}
		}
	}

	assert!(
		missing.is_empty(),
		"flags accepted by the CLI but absent from docs/commands.md:\n  {}",
		missing.join("\n  ")
	);
}

/// The global options table is written separately from the per-command ones, so
/// it drifts separately too.
#[test]
fn every_global_flag_is_documented() {
	let docs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/commands.md"))
		.expect("read docs/commands.md");
	let out = Command::new(bin())
		.arg("--help")
		.output()
		.expect("run --help");
	let help = String::from_utf8_lossy(&out.stdout);

	let missing: Vec<String> = flags_in_help(&help)
		.into_iter()
		.filter(|f| !docs.contains(f))
		.collect();
	assert!(
		missing.is_empty(),
		"global flags absent from docs/commands.md:\n  {}",
		missing.join("\n  ")
	);
}

/// The CLI names the open standard, not another vendor's product.
///
/// `podup` implements the **Compose Spec**, which is a published standard with
/// its own name, so saying "Compose" is both brand-free and more accurate than
/// naming a particular implementation of it.
///
/// The one allowed exception is a literal filename. `docker-compose.yaml` is
/// what the file on disk is called, the same way `.gitignore` is, and the `-f`
/// help has to say which names it probes or it is hiding real behaviour.
/// Removing it would not remove a brand mention; it would remove a fact.
///
/// Compatibility itself is untouched by any of this: it lives in what the flags
/// do, not in what the help text calls them.
#[test]
fn the_cli_help_names_no_other_vendor() {
	let mut offenders: Vec<String> = Vec::new();
	let mut check = |label: &str, text: &str| {
		for line in text.lines() {
			let lower = line.to_ascii_lowercase();
			if !lower.contains("docker") {
				continue;
			}
			// Filenames are data, not branding.
			if lower.contains("docker-compose.yaml") || lower.contains("docker-compose.yml") {
				continue;
			}
			offenders.push(format!("{label}: {}", line.trim()));
		}
	};

	let out = Command::new(bin())
		.arg("--help")
		.output()
		.expect("run --help");
	check("(top level)", &String::from_utf8_lossy(&out.stdout));
	for cmd in COMMANDS {
		let out = Command::new(bin())
			.args([cmd, "--help"])
			.output()
			.unwrap_or_else(|e| panic!("run {cmd} --help: {e}"));
		check(cmd, &String::from_utf8_lossy(&out.stdout));
	}

	assert!(
		offenders.is_empty(),
		"the CLI help should name the Compose Spec, not a vendor:\n  {}",
		offenders.join("\n  ")
	);
}
