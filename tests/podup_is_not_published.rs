//! podup is distributed as a binary, not as a crates.io library.
//!
//! It was published for helmly-agent, and helmly-agent does not use it: zero
//! `use podup::`, zero entries in its `Cargo.lock`, and its own manifest records
//! that the dependency is gone. When it did exist it was a git dependency, so
//! crates.io was never what carried it, and crates.io reports zero reverse
//! dependencies. What publishing cost was a semver promise on seventeen `pub`
//! items with nobody to make it to, plus two required status checks.
//!
//! `publish = false` is the part cargo enforces. These are the parts it does
//! not: that the key survives, and that no workflow here reaches around it.

use std::fs;
use std::path::Path;

fn repo(rel: &str) -> String {
	let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
	fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

#[test]
fn the_manifest_says_it_is_not_published() {
	let manifest = repo("Cargo.toml");
	// Anchored to the `[package]` table: `Cargo.toml` has more than one table
	// and a `publish` key elsewhere would not mean this.
	let package = manifest
		.split_once("[package]")
		.map(|(_, rest)| rest.split_once("\n[").map(|(t, _)| t).unwrap_or(rest))
		.unwrap_or_else(|| panic!("no [package] table in Cargo.toml"));
	assert!(
		package.lines().any(|l| l.trim() == "publish = false"),
		"[package] no longer carries `publish = false`, so `cargo publish` would \
		 accept this crate again. If that is intended, the README's Design section \
		 and this test are what say otherwise and both need updating"
	);
}

#[test]
fn no_workflow_owned_here_runs_cargo_publish() {
	// `reusable-rust-ci.yml` keeps a guarded `cargo publish --dry-run` behind
	// `if: inputs.package-check`, which `ci.yml` now passes as false. It is
	// excluded because it is a generic reusable rather than a decision about
	// podup — the thing worth catching is a caller turning it back on, or a new
	// step publishing for real.
	let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
	let mut offenders = Vec::new();
	for entry in fs::read_dir(&dir).expect(".github/workflows is readable") {
		let path = entry.expect("entry is readable").path();
		let name = path.file_name().unwrap().to_string_lossy().into_owned();
		if name == "reusable-rust-ci.yml" {
			continue;
		}
		let Ok(text) = fs::read_to_string(&path) else {
			continue;
		};
		for line in text.lines() {
			let t = line.trim();
			if t.starts_with('#') {
				continue;
			}
			if t.contains("cargo publish") || t.contains("package-check: true") {
				offenders.push(format!("{name}: {t}"));
			}
		}
	}
	assert!(
		offenders.is_empty(),
		"podup is not published, and these would publish it or re-enable the \
		 packaging check that assumes it is: {offenders:#?}"
	);
}

/// The claim that helmly-agent consumed the library lived in `README.md` **and**
/// `CONTRIBUTING.md`, and fixing the first did not fix the second. Prose does not
/// have one home, so grepping the file you happen to be editing is not a check.
///
/// Deliberately narrow: these three markers, not the words "crates.io", which the
/// docs use correctly all over — the threat model lists it as untrusted network,
/// and the packaging notes explain why podup is not on it.
#[test]
fn no_document_still_says_the_library_is_published_or_consumed() {
	const STALE: [&str; 3] = [
		"crate is consumed by",
		"docs.rs/podup",
		"cargo install podup",
	];

	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let mut files: Vec<std::path::PathBuf> =
		vec![root.join("README.md"), root.join("CONTRIBUTING.md")];
	// `fs::read_dir` rather than `git ls-files`: the Debian package build has no
	// git binary, and a test that cannot run there is a test that fails there.
	for entry in fs::read_dir(root.join("docs")).expect("docs/ is readable") {
		let path = entry.expect("entry is readable").path();
		if path.extension().is_some_and(|e| e == "md") {
			files.push(path);
		}
	}
	assert!(
		files.len() > 3,
		"too few documents found; the scan is reading the wrong tree"
	);

	let mut offenders = Vec::new();
	for f in &files {
		let Ok(text) = fs::read_to_string(f) else {
			continue;
		};
		let name = f.file_name().unwrap().to_string_lossy().into_owned();
		for marker in STALE {
			if text.contains(marker) {
				offenders.push(format!("{name}: {marker:?}"));
			}
		}
	}
	assert!(
		offenders.is_empty(),
		"podup is not published, so these read as promises it no longer keeps: {offenders:#?}"
	);
}
