//! Pattern-matching cases split out of `tests.rs`, which reached the
//! 500-line hard limit. These exercise `is_ignored`, `glob_match` and
//! `to_ignore_path` against strings only; what stays in `tests.rs` builds
//! a real context tar and asserts on its entries. The split follows that
//! seam rather than a line count.

use super::*;

#[test]
fn build_ignored_exact() {
	let patterns = vec!["secret.txt".to_string()];
	assert!(is_ignored("secret.txt", &patterns));
	assert!(!is_ignored("secret.txt.bak", &patterns));
}
#[test]
fn build_ignored_dir() {
	let patterns = vec!["node_modules/".to_string()];
	assert!(is_ignored("node_modules/foo.js", &patterns));
	assert!(!is_ignored("other/foo.js", &patterns));
}
#[test]
fn build_ignored_path_separator() {
	let patterns = vec!["vendor".to_string()];
	assert!(is_ignored("vendor/lib.rs", &patterns));
	assert!(!is_ignored("notvendor/lib.rs", &patterns));
}
#[test]
fn build_ignored_glob_extension() {
	let patterns = vec!["*.key".to_string()];
	assert!(is_ignored("secret.key", &patterns));
	assert!(is_ignored("certs/ca.key", &patterns));
	assert!(!is_ignored("key.txt", &patterns));
}
#[test]
fn build_ignored_glob_in_subdir() {
	let patterns = vec!["logs/*.log".to_string()];
	assert!(is_ignored("logs/error.log", &patterns));
	assert!(!is_ignored("other/error.log", &patterns));
}
#[test]
fn glob_match_star_extension() {
	assert!(glob_match("*.env", "production.env"));
	assert!(glob_match("*.env", "config/.env"));
	assert!(!glob_match("*.env", "env.txt"));
}
#[test]
fn glob_match_star_prefix() {
	assert!(glob_match("id_*", "id_rsa"));
	assert!(glob_match("id_*", "id_ed25519"));
	assert!(!glob_match("id_*", "not_id_rsa"));
}
#[test]
fn glob_match_double_star_any_depth() {
	assert!(glob_match("**/*.key", "secret.key"));
	assert!(glob_match("**/*.key", "a/b/c/secret.key"));
	assert!(glob_match("a/**/b", "a/b"));
	assert!(glob_match("a/**/b", "a/x/y/b"));
	assert!(!glob_match("a/**/b", "z/b"));
}
#[test]
fn glob_match_question_mark() {
	assert!(glob_match("file?.txt", "file1.txt"));
	assert!(!glob_match("file?.txt", "file.txt"));
	assert!(!glob_match("file?.txt", "file12.txt"));
}
#[test]
fn dockerignore_negation_reincludes() {
	let patterns = vec!["*.log".to_string(), "!keep.log".to_string()];
	assert!(is_ignored("error.log", &patterns));
	assert!(!is_ignored("keep.log", &patterns));
}
#[test]
fn dockerignore_negation_order_matters() {
	// Re-include then exclude again: last match wins.
	let patterns = vec![
		"logs/".to_string(),
		"!logs/keep/".to_string(),
		"logs/keep/secret.txt".to_string(),
	];
	assert!(is_ignored("logs/a.log", &patterns));
	assert!(!is_ignored("logs/keep/b.log", &patterns));
	assert!(is_ignored("logs/keep/secret.txt", &patterns));
}
#[test]
fn build_ignored_empty_pattern_matches_nothing() {
	// A blank `.dockerignore` line yields an empty pattern that must never
	// match (otherwise it would exclude every file).
	let patterns = vec![String::new()];
	assert!(!is_ignored("anything.txt", &patterns));
	assert!(!is_ignored("a/b/c", &patterns));
}
#[test]
fn glob_match_double_star_suffix_spans_subtree() {
	// A trailing `**` matches the directory and everything beneath it.
	assert!(glob_match("build/**", "build/out.o"));
	assert!(glob_match("build/**", "build/a/b/out.o"));
	assert!(!glob_match("build/**", "src/out.o"));
}
#[test]
fn glob_match_double_star_middle_with_no_match_fails() {
	// `a/**/z` requires the path to start with `a/` and end with `z`; a path
	// that never reaches the trailing literal exhausts the `**` prefix loop and
	// fails rather than matching loosely.
	assert!(glob_match("a/**/z", "a/b/c/z"));
	assert!(!glob_match("a/**/z", "a/b/c/y"));
}
#[test]
fn glob_match_question_mark_matches_single_non_slash_char() {
	// `?` matches exactly one character and never a path separator.
	assert!(glob_match("file?.txt", "file1.txt"));
	assert!(!glob_match("file?.txt", "file.txt"));
	assert!(!glob_match("a?b", "a/b"));
}
#[test]
fn ignore_matching_uses_forward_slashes_on_every_platform() {
	// `.dockerignore` patterns always use `/`. `Path` yields `\` on Windows,
	// so matching the raw string meant nothing below the top level was ever
	// ignored there and `vendor/` silently did nothing. The tar writer
	// already normalises, so the entry names and the ignore check disagreed
	// about the same file. Caught by the negation case failing on the
	// Windows runner and passing everywhere else.
	let rel = std::path::Path::new("vendor").join("drop.txt");
	assert_eq!(
		super::to_ignore_path(&rel),
		"vendor/drop.txt",
		"the ignore path must be slash-separated whatever the platform uses"
	);
	assert!(super::is_ignored(
		&super::to_ignore_path(&rel),
		&["vendor/".to_string()]
	));
}
