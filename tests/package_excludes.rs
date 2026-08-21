//! Every top-level directory is either published or named in `exclude`.
//!
//! `cargo package` publishes the whole tree minus `[package] exclude`, so a new
//! top-level directory ships the day it is added and nobody is told. That is how
//! `bench/` came to be in the crate: `Cargo.toml` carries a second `exclude`
//! under `[workspace]` which names `bench/timeit` and `fuzz`, and reading it
//! answers a different question than the one it looks like it answers.
//!
//! This asserts the decision was taken, not that it went a particular way. A new
//! directory has to be added to one of the two lists below, which is a line in a
//! pull request rather than a surprise in a published crate.
//!
//! It reads the manifest instead of running `cargo package`, so it costs
//! milliseconds and needs no network.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Top-level directories whose contents belong in the published crate.
const PUBLISHED: &[&str] = &["internal"];

/// Top-level directories that carry their own manifest at the top level.
/// `cargo package` does not descend into a nested package, so these are absent
/// from the crate whether or not `exclude` names them — which is why adding
/// `fuzz` to `exclude` closed nothing when it was tried.
///
/// `bench` is deliberately NOT here. Its manifest is one level down, at
/// `bench/timeit/Cargo.toml`, so `cargo package` descends into `bench/` and
/// publishes everything beside that nested package: 30 files, measured. Listing
/// it here was the first version of this file, and deleting `/bench` from the
/// manifest left the test green — a control that can be removed without the
/// test noticing is not a control.
const NESTED_PACKAGES: &[&str] = &["fuzz"];

/// Read the `exclude` array that belongs to `[package]`, stopping at the next
/// table header so a later `[workspace] exclude` cannot be mistaken for it.
fn package_excludes(manifest: &str) -> BTreeSet<String> {
	let mut out = BTreeSet::new();
	let mut in_package = true;
	let mut in_exclude = false;
	for line in manifest.lines() {
		let trimmed = line.trim();
		if trimmed.starts_with('[') {
			in_package = trimmed == "[package]";
			in_exclude = false;
			continue;
		}
		if !in_package {
			continue;
		}
		if let Some(rest) = trimmed.strip_prefix("exclude") {
			let rest = rest.trim_start_matches([' ', '=']).trim();
			if rest == "[" {
				in_exclude = true;
			} else {
				for entry in rest.trim_matches(['[', ']']).split(',') {
					let entry = entry.trim().trim_matches('"');
					if !entry.is_empty() {
						out.insert(entry.trim_start_matches('/').to_string());
					}
				}
			}
			continue;
		}
		if in_exclude {
			if trimmed.starts_with(']') {
				in_exclude = false;
				continue;
			}
			let entry = trimmed.trim_end_matches(',').trim().trim_matches('"');
			if !entry.is_empty() {
				out.insert(entry.trim_start_matches('/').to_string());
			}
		}
	}
	out
}

#[test]
fn every_top_level_directory_is_a_decision() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let excluded = package_excludes(&fs::read_to_string(root.join("Cargo.toml")).unwrap());

	// Ask git, not the filesystem. Inside a checkout `cargo package` takes its
	// file list from git, so an untracked build directory in the working tree is
	// never published — and reading `read_dir` here would fail the test on every
	// machine that has one, which is the instrument measuring something other
	// than the question.
	let listing = Command::new("git")
		.args(["ls-tree", "-d", "--name-only", "HEAD"])
		.current_dir(root)
		.output();
	let Ok(listing) = listing else {
		eprintln!("git is unavailable, so there is no file list to check");
		return;
	};
	assert!(
		listing.status.success(),
		"git ls-tree failed: {}",
		String::from_utf8_lossy(&listing.stderr)
	);

	let mut undecided = Vec::new();
	for name in String::from_utf8_lossy(&listing.stdout).lines() {
		let name = name.trim().to_string();
		if name.is_empty() || name.starts_with('.') {
			continue;
		}
		if PUBLISHED.contains(&name.as_str())
			|| NESTED_PACKAGES.contains(&name.as_str())
			|| excluded.contains(&name)
		{
			continue;
		}
		undecided.push(name);
	}

	assert!(
		undecided.is_empty(),
		"these top-level directories are neither published, a nested package, nor in \
		 the [package] exclude list, so `cargo package` ships them: {undecided:?}. \
		 Add each to Cargo.toml's [package] exclude, or to PUBLISHED in this file."
	);
}

#[test]
fn the_manifest_reader_stops_at_the_next_table() {
	// The bug this file exists for: two `exclude` keys, and the wrong one is
	// the easier one to read. A reader that ran to the end of the file would
	// report `bench/timeit` as excluded from the package, which it is not.
	let manifest =
		"[package]\nexclude = [\"/docs\"]\n\n[workspace]\nexclude = [\"bench/timeit\", \"fuzz\"]\n";
	let excluded = package_excludes(manifest);
	assert!(
		excluded.contains("docs"),
		"the [package] entry must be read"
	);
	assert!(
		!excluded.contains("bench/timeit"),
		"the [workspace] entry must not be read as a packaging decision"
	);
}
