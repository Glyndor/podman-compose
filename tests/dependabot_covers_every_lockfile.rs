//! Every Cargo lockfile in the tree is in Dependabot's rotation.
//!
//! `Cargo.toml` excludes `fuzz` and `bench/timeit` from the workspace, so each
//! resolves its own dependencies into its own `Cargo.lock`. Dependabot's cargo
//! entry names a `directory`, and the one for `/` does not reach either. Both
//! sat unwatched, which is not a cosmetic gap: a lockfile nothing updates keeps
//! whatever advisory it was pinned to, and `audit.yml` runs `cargo audit`
//! against the workspace lock rather than these.
//!
//! Nothing said so. The exclusion is one line in `Cargo.toml` and the coverage
//! is three lines in another file, and adding a fourth excluded crate would be
//! silent in exactly the same way. This is the sentence that fails instead.

use std::fs;
use std::path::Path;
use std::process::Command;

fn repo(rel: &str) -> String {
	let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
	fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// The `directory:` of every `package-ecosystem: "cargo"` block.
///
/// Hand-parsed rather than pulled through a YAML crate: the file is ours, the
/// shape is two keys, and a dev-dependency added to read three lines would be a
/// worse trade than a scanner whose failure mode is a test that says so.
fn cargo_directories(yaml: &str) -> Vec<String> {
	let mut dirs = Vec::new();
	let mut in_cargo = false;
	for line in yaml.lines() {
		let t = line.trim();
		if t.starts_with("- package-ecosystem:") {
			in_cargo = t.contains("\"cargo\"") || t.contains("'cargo'");
		} else if in_cargo && t.starts_with("directory:") {
			let v = t
				.trim_start_matches("directory:")
				.trim()
				.trim_matches(['"', '\'']);
			dirs.push(v.to_string());
			in_cargo = false;
		}
	}
	dirs
}

/// Every tracked `Cargo.lock`, as the directory Dependabot would name for it.
fn tracked_lockfile_directories() -> Vec<String> {
	let out = Command::new("git")
		.args([
			"ls-files",
			"-z",
			"Cargo.lock",
			"*/Cargo.lock",
			"**/Cargo.lock",
		])
		.current_dir(env!("CARGO_MANIFEST_DIR"))
		.output()
		.expect("git ls-files runs");
	assert!(out.status.success(), "git ls-files failed: {out:?}");
	let mut dirs: Vec<String> = String::from_utf8_lossy(&out.stdout)
		.split('\0')
		.filter(|p| !p.is_empty())
		.map(|p| match p.rsplit_once('/') {
			Some((parent, _)) => format!("/{parent}"),
			None => "/".to_string(),
		})
		.collect();
	dirs.sort();
	dirs.dedup();
	dirs
}

#[test]
fn every_cargo_lockfile_has_a_dependabot_entry() {
	let declared = cargo_directories(&repo(".github/dependabot.yml"));
	let present = tracked_lockfile_directories();

	// A scanner that found nothing would pass this test by reporting no gaps,
	// which is the failure this repository keeps meeting. Both sides get a
	// floor: the tree has at least the workspace lock, and the config has at
	// least the entry for it.
	assert!(
		present.contains(&"/".to_string()),
		"no workspace Cargo.lock found — the scanner is reading the wrong tree: {present:?}"
	);
	assert!(
		declared.contains(&"/".to_string()),
		"no cargo entry for / — the parser is not reading dependabot.yml: {declared:?}"
	);

	let missing: Vec<&String> = present.iter().filter(|d| !declared.contains(d)).collect();
	assert!(
		missing.is_empty(),
		"these Cargo.lock files are in the tree and in no Dependabot cargo entry, so nothing \
		 ever updates them: {missing:?}. Declared: {declared:?}"
	);
}

#[test]
fn no_dependabot_cargo_entry_points_at_a_directory_with_no_lockfile() {
	// The other direction, and the cheaper mistake: an entry left behind after a
	// crate is deleted or folded back into the workspace. Dependabot does not
	// fail on it, it just never opens anything, which reads identically to a
	// crate with no updates available.
	let declared = cargo_directories(&repo(".github/dependabot.yml"));
	let present = tracked_lockfile_directories();
	let stale: Vec<&String> = declared.iter().filter(|d| !present.contains(d)).collect();
	assert!(
		stale.is_empty(),
		"these Dependabot cargo entries name a directory with no tracked Cargo.lock: {stale:?}"
	);
}
