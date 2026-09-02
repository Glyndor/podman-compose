//! Rust unit tests live in sibling files, never in an inline module.
//!
//! `cargo-llvm-cov` counts the body of an inline `#[cfg(test)] mod tests { … }`
//! as covered lines of the production file it sits in, so the coverage gate
//! reads test volume as reach, and `line-limit` pushes the same file toward
//! its cap for its tests. Measured on this tree on 2026-09-02: moving 103
//! inline modules to siblings took the figure from 82.08% to 73.89% with the
//! same 1762 unit tests. The convention is a sibling `foo_tests.rs` declared
//! with `#[cfg(test)] #[path = "foo_tests.rs"] mod tests;` (standards/structure).
//!
//! This test is what keeps the number honest after the migration: a new
//! inline module fails it by file and line, with the fix.

use std::fs;
use std::path::Path;

fn walk(dir: &Path, out: &mut Vec<String>) {
	for entry in fs::read_dir(dir).expect("read dir") {
		let path = entry.expect("dir entry").path();
		if path.is_dir() {
			walk(&path, out);
			continue;
		}
		if path.extension().map(|e| e == "rs") != Some(true) {
			continue;
		}
		// A sibling test file may nest its own modules; everything in it is
		// already outside the measurement, which is what the rule is about.
		if path
			.file_name()
			.and_then(|n| n.to_str())
			.map(|n| n.ends_with("tests.rs"))
			== Some(true)
		{
			continue;
		}
		let src = fs::read_to_string(&path).expect("read source");
		let lines: Vec<&str> = src.lines().collect();
		for (i, line) in lines.iter().enumerate() {
			// A module OPENED with a brace, under a test cfg. `mod tests;` (a
			// sibling) has no brace and is the shape wanted.
			let trimmed = line.trim_start();
			if !(trimmed.starts_with("mod ") && trimmed.ends_with('{')) {
				continue;
			}
			let under_test_cfg = lines[..i]
				.iter()
				.rev()
				.take_while(|l| l.trim_start().starts_with("#["))
				.any(|l| l.contains("cfg(") && l.contains("test"));
			if under_test_cfg {
				out.push(format!("{}:{}: `{}`", path.display(), i + 1, trimmed));
			}
		}
	}
}

#[test]
fn no_inline_test_module_under_internal() {
	let mut found = Vec::new();
	walk(
		Path::new(env!("CARGO_MANIFEST_DIR"))
			.join("internal")
			.as_path(),
		&mut found,
	);
	assert!(
		found.is_empty(),
		"inline test module(s) under internal/; move the body to a sibling `<stem>_tests.rs` \
		 and declare `#[cfg(test)] #[path = \"<stem>_tests.rs\"] mod tests;` instead:\n  {}",
		found.join("\n  ")
	);
}

/// The control: the detector sees the shape it is written against. Without
/// this a walker that matched nothing would agree with any tree.
#[test]
fn the_detector_reports_a_planted_inline_module() {
	let dir = tempfile::tempdir().expect("tempdir");
	fs::write(
		dir.path().join("planted.rs"),
		"fn prod() {}\n\n#[cfg(test)]\nmod tests {\n\t#[test]\n\tfn t() {}\n}\n",
	)
	.expect("write");
	fs::write(
		dir.path().join("sibling.rs"),
		"fn prod() {}\n\n#[cfg(test)]\n#[path = \"sibling_tests.rs\"]\nmod tests;\n",
	)
	.expect("write");
	fs::write(
		dir.path().join("plain.rs"),
		"mod inner {\n\tpub fn f() {}\n}\n",
	)
	.expect("write");
	let mut found = Vec::new();
	walk(dir.path(), &mut found);
	assert_eq!(found.len(), 1, "exactly the planted module: {found:?}");
	assert!(found[0].contains("planted.rs:4"), "{found:?}");
}
