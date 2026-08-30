//! Every crate with its own lockfile is in Dependabot's rotation.
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

/// The directories Dependabot has to name, derived from the cause rather than
/// from the tree.
///
/// A crate listed in `[workspace] exclude` resolves its own dependencies into
/// its own `Cargo.lock`, which is exactly why the entry for `/` does not reach
/// it. Reading that list is therefore the same question as finding the
/// lockfiles, and it survives environments the tree does not: the first version
/// of this shelled out to `git ls-files`, and the Debian package build has no
/// `git` binary at all, so both tests died with `NotFound` inside
/// `dpkg-buildpackage` rather than reporting anything.
fn directories_needing_an_entry() -> Vec<String> {
	let manifest = repo("Cargo.toml");

	// Anchored to the `[workspace]` table on purpose. `Cargo.toml` carries a
	// second `exclude`, under `[package]`, listing what `cargo package` leaves
	// out of the crate archive — and the file says so in a comment right above
	// it: "Not to be confused with `[workspace] exclude`". Matching the first
	// `exclude` in the file finds that one, whose entries are not crates, so the
	// derived list comes back as just `/` and the coverage test passes by having
	// nothing to check.
	let mut in_workspace = false;
	let mut list = String::new();
	let mut collecting = false;
	for line in manifest.lines() {
		let t = line.trim();
		if t.starts_with('[') && !collecting {
			in_workspace = t == "[workspace]";
			continue;
		}
		if in_workspace && t.starts_with("exclude") {
			collecting = true;
		}
		if collecting {
			list.push_str(t);
			if t.contains(']') {
				break;
			}
		}
	}

	let inner = list
		.split_once('[')
		.map(|(_, rest)| rest.split_once(']').map(|(i, _)| i).unwrap_or(""))
		.unwrap_or("");

	let mut dirs = vec!["/".to_string()];
	for raw in inner.split(',') {
		let name = raw.trim().trim_matches('"');
		if name.is_empty() {
			continue;
		}
		// An excluded path is only a crate if it has a manifest of its own.
		if Path::new(env!("CARGO_MANIFEST_DIR"))
			.join(name)
			.join("Cargo.toml")
			.is_file()
		{
			dirs.push(format!("/{name}"));
		}
	}
	dirs.sort();
	dirs
}

#[test]
fn every_cargo_lockfile_has_a_dependabot_entry() {
	let declared = cargo_directories(&repo(".github/dependabot.yml"));
	let present = directories_needing_an_entry();

	// A scanner that found nothing would pass this test by reporting no gaps,
	// which is the failure this repository keeps meeting. Both sides get a
	// floor: the tree has at least the workspace lock, and the config has at
	// least the entry for it.
	assert!(
		present.contains(&"/".to_string()),
		"no workspace entry derived — the scanner is reading the wrong tree: {present:?}"
	);
	// The floor that matters, and the one the first version of this test did not
	// have. `Cargo.toml` excludes two crates, so a derived list of just `/` means
	// the parser broke rather than that there is nothing to cover — and with
	// nothing to cover, the comparison below passes while checking nothing. That
	// is exactly how the first version passed against the wrong `exclude` key.
	assert!(
		present.len() > 1,
		"only the workspace root was derived from [workspace] exclude, so this test would \
		 pass without checking anything. The parser is broken, or the exclusions are gone \
		 and this assertion is what should be updated: {present:?}"
	);
	assert!(
		declared.contains(&"/".to_string()),
		"no cargo entry for / — the parser is not reading dependabot.yml: {declared:?}"
	);

	let missing: Vec<&String> = present.iter().filter(|d| !declared.contains(d)).collect();
	assert!(
		missing.is_empty(),
		"these crates are excluded from the workspace, so they carry their own Cargo.lock, \
		 and no Dependabot cargo entry names them — nothing ever updates them: {missing:?}. \
		 Declared: {declared:?}"
	);
}

#[test]
fn no_dependabot_cargo_entry_points_at_a_directory_with_no_lockfile() {
	// The other direction, and the cheaper mistake: an entry left behind after a
	// crate is deleted or folded back into the workspace. Dependabot does not
	// fail on it, it just never opens anything, which reads identically to a
	// crate with no updates available.
	let declared = cargo_directories(&repo(".github/dependabot.yml"));
	let present = directories_needing_an_entry();
	let stale: Vec<&String> = declared.iter().filter(|d| !present.contains(d)).collect();
	assert!(
		stale.is_empty(),
		"these Dependabot cargo entries name a directory that is neither the workspace root \
		 nor an excluded crate: {stale:?}"
	);
}
