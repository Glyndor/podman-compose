//! The release's dependency audit must be at least as strict as the weekly one.
//!
//! Two workflows run `cargo audit` and the strictness flag is written in both,
//! derived from neither. Before #1592 they disagreed: `reusable-rust-audit.yml`
//! ran `--deny warnings` and `release.yml` did not, so the gate standing in
//! front of an immutable artifact was the more permissive of the two. An
//! unmaintained or yanked crate is a warning, and a release is the one place a
//! warning cannot be looked at again later.
//!
//! The asymmetry with pull requests is deliberate and is not what this file
//! asserts: an upstream advisory must not block an unrelated pull request, so
//! `audit.yml`'s pull-request trigger is scoped to the manifest and the
//! lockfile. A blocked release is fixed by releasing again. A published one
//! cannot be fixed at all.

use std::fs;
use std::path::Path;

fn read(name: &str) -> String {
	let path = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join(".github/workflows")
		.join(name);
	fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name} is readable: {e}"))
}

/// Every line that INVOKES `cargo audit`, with its flags.
///
/// Two shapes are excluded and both are real. A comment line: both files
/// explain the flag in prose directly above the line that carries it, so a
/// search for the string finds it whether or not the command has it. And a
/// YAML key: `reusable-rust-audit.yml` names its job `cargo audit`, which is a
/// label, not a command. The first draft of this parser counted that label and
/// the test went red against a file that was already correct.
///
/// So the rule is positional: the trimmed line must BEGIN the command, either
/// on its own inside a `run: |` block or immediately after `run:`.
fn audit_invocations(workflow: &str) -> Vec<&str> {
	workflow
		.lines()
		.map(str::trim)
		.filter(|l| !l.starts_with('#'))
		.filter(|l| l.starts_with("cargo audit") || l.starts_with("run: cargo audit"))
		.collect()
}

#[test]
fn every_cargo_audit_denies_warnings() {
	for name in ["release.yml", "reusable-rust-audit.yml"] {
		let body = read(name);
		let calls = audit_invocations(&body);
		assert!(
			!calls.is_empty(),
			"{name} runs no cargo audit; either the step moved and this test is \
			 reading the wrong file, or the gate is gone"
		);
		for call in calls {
			assert!(
				call.contains("--deny warnings"),
				"{name} runs {call:?} without --deny warnings. The two gates \
				 must agree on strictness: whichever is weaker is the one that \
				 decides, and for the release that decision is immutable."
			);
		}
	}
}

#[test]
fn the_pull_request_audit_is_scoped_to_the_manifest() {
	let body = read("audit.yml");
	assert!(
		body.contains("pull_request:"),
		"audit.yml no longer runs on pull requests, so a change that introduces \
		 a vulnerable dependency waits up to a week for the Monday cron"
	);
	for path in ["\"Cargo.lock\"", "\"Cargo.toml\""] {
		assert!(
			body.contains(path),
			"audit.yml's pull-request trigger does not name {path}. Without the \
			 manifest in its paths filter it either misses the change that \
			 introduces a dependency, or runs on every pull request and lets \
			 somebody else's disclosure timing block work that did not cause it."
		);
	}
	assert!(
		body.contains("paths:"),
		"audit.yml runs on every pull request with no paths filter. An upstream \
		 advisory published today would then block every open pull request, \
		 which is the case standards/testing rules out by name."
	);
}

/// The parser is what both tests trust. Pin it on input that differs from the
/// real files, including the shape it must not count.
#[test]
fn the_parser_ignores_the_prose_that_explains_the_flag() {
	let yml = "\
  audit:
    name: cargo audit
    steps:
      - name: Audit
        # cargo audit --deny warnings, explained here and not run
        run: |
          cargo audit --deny warnings
";
	let calls = audit_invocations(yml);
	assert_eq!(
		calls,
		vec!["cargo audit --deny warnings"],
		"exactly one line invokes it. The other two name it: a job label \
		 `name: cargo audit`, which is what the first draft of this parser \
		 counted, and a comment explaining the flag."
	);
}
