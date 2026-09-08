//! Unit tests for the build-context tar assembly and `.dockerignore`
//! matching in [`super`] (split out to keep the module under the source
//! line limit).

use super::{
	build_context_tar, build_context_tar_with_inline, glob_match, ignore_file, is_ignored,
	map_additional_context,
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

#[test]
fn additional_context_prefix_mapping() {
	let base = Path::new("/proj");
	assert_eq!(
		map_additional_context(base, "docker-image://alpine"),
		"image:alpine"
	);
	assert_eq!(
		map_additional_context(base, "https://example.org/ctx.tar"),
		"url:https://example.org/ctx.tar"
	);
	assert_eq!(
		map_additional_context(base, "git://example.org/r.git"),
		"url:git://example.org/r.git"
	);
	// Local paths are resolved against base_dir using the host's native path
	// separator, so compare against a `join`ed path rather than a literal.
	let local = map_additional_context(base, "sub/dir");
	assert!(local.starts_with("localpath:"));
	assert!(local.ends_with(&base.join("sub/dir").display().to_string()));
}

#[test]
fn extra_files_are_added_to_context_tar() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	let extra = vec![(".podup-build-secret-tok".to_string(), b"hunter2".to_vec())];
	let bytes = build_context_tar(dir.path(), "Dockerfile", &extra).unwrap();

	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	let names: Vec<String> = archive
		.entries()
		.unwrap()
		.filter_map(|e| e.ok())
		.filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
		.collect();
	assert!(
		names.iter().any(|n| n.contains(".podup-build-secret-tok")),
		"secret entry must be present: {names:?}"
	);
}

#[test]
fn build_secret_entries_excluded_from_copy_via_dockerignore() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\nCOPY . /app\n").unwrap();
	fs::write(dir.path().join(".dockerignore"), b"*.log\n").unwrap();
	let extra = vec![(".podup-build-secret-tok".to_string(), b"hunter2".to_vec())];
	let bytes = build_context_tar(dir.path(), "Dockerfile", &extra).unwrap();

	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	let mut di_entries = 0;
	let mut di = String::new();
	for entry in archive.entries().unwrap() {
		let mut entry = entry.unwrap();
		if entry.path().unwrap().to_string_lossy() == ".dockerignore" {
			di_entries += 1;
			entry.read_to_string(&mut di).unwrap();
		}
	}
	assert_eq!(di_entries, 1, "exactly one .dockerignore entry");
	assert!(di.lines().any(|l| l == "*.log"), "user rule kept: {di:?}");
	assert!(
		di.lines().any(|l| l == ".podup-build-secret-tok"),
		"secret entry must be COPY-excluded via .dockerignore: {di:?}"
	);
}

#[test]
fn inline_tar_excludes_build_secrets_via_dockerignore() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("app.txt"), b"x").unwrap();
	let extra = vec![(".podup-build-secret-db".to_string(), b"s3cret".to_vec())];
	let (bytes, _) = build_context_tar_with_inline(dir.path(), "FROM alpine\n", &extra).unwrap();

	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	let mut di = String::new();
	for entry in archive.entries().unwrap() {
		let mut entry = entry.unwrap();
		// No user ignore file in this context, so the synthesized one is
		// written as `.containerignore`, the name podman prefers, so a
		// stray `.dockerignore` cannot shadow our exclusions.
		if entry.path().unwrap().to_string_lossy() == ".containerignore" {
			entry.read_to_string(&mut di).unwrap();
		}
	}
	assert!(
		di.lines().any(|l| l == ".dockerfile-inline"),
		"inline exclusion kept: {di:?}"
	);
	assert!(
		di.lines().any(|l| l == ".podup-build-secret-db"),
		"secret entry must be COPY-excluded via .dockerignore: {di:?}"
	);
}

#[test]
fn dockerfile_is_force_included_despite_dockerignore() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	// A `.dockerignore` that matches the Dockerfile (here a blanket `*`) must
	// not drop it from the context tar; Docker keeps the active Dockerfile
	// available to the builder regardless. Without the force-include the build
	// fails with "stat .../Dockerfile: no such file or directory".
	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	fs::write(dir.path().join(".dockerignore"), b"*\n").unwrap();
	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();

	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let names: Vec<String> = tar::Archive::new(raw.as_slice())
		.entries()
		.unwrap()
		.filter_map(|e| e.ok())
		.filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
		.collect();
	assert!(
		names.iter().any(|n| n == "Dockerfile"),
		"Dockerfile must survive a `*` .dockerignore: {names:?}"
	);
}

// is_ignored (build) ---------------------------------------------------

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

// ignore_file ----------------------------------------------------------

#[test]
fn dockerignore_parsed_correctly() {
	let dir = tempdir().unwrap();
	fs::write(
		dir.path().join(".dockerignore"),
		b"# comment\n\ntarget/\n*.log\n",
	)
	.unwrap();
	let (name, patterns) = ignore_file(dir.path());
	assert_eq!(name, ".dockerignore");
	assert_eq!(patterns, vec!["target/", "*.log"]);
}

#[test]
fn containerignore_is_read_when_it_is_the_only_one() {
	let dir = tempdir().unwrap();
	fs::write(dir.path().join(".containerignore"), b"secrets/\n*.key\n").unwrap();
	let (name, patterns) = ignore_file(dir.path());
	assert_eq!(name, ".containerignore");
	assert_eq!(patterns, vec!["secrets/", "*.key"]);
}

/// podman-build(1): "if both are in the context directory, podman build only
/// uses `.containerignore`". Applying both client-side is what produced an image
/// missing content that `podman build` includes.
#[test]
fn containerignore_wins_and_dockerignore_is_not_merged() {
	let dir = tempdir().unwrap();
	fs::write(dir.path().join(".containerignore"), b"a.txt\n").unwrap();
	fs::write(dir.path().join(".dockerignore"), b"b.txt\n").unwrap();
	let (name, patterns) = ignore_file(dir.path());
	assert_eq!(name, ".containerignore");
	assert_eq!(
		patterns,
		vec!["a.txt"],
		"the two files must never be unioned"
	);
}

/// With neither present the name still matters: it is what a synthesized ignore
/// file is written under, and podman prefers `.containerignore`, so choosing it
/// means our exclusions cannot be shadowed by a `.dockerignore`.
#[test]
fn no_ignore_file_defaults_to_containerignore_with_no_patterns() {
	let dir = tempdir().unwrap();
	let (name, patterns) = ignore_file(dir.path());
	assert_eq!(name, ".containerignore");
	assert!(patterns.is_empty());
}

// build_context_tar ----------------------------------------------------

#[test]
fn context_tar_produces_valid_gzip() {
	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	fs::write(dir.path().join("app.rs"), b"fn main() {}").unwrap();
	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();
	assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
}

#[test]
fn context_tar_excludes_dockerignore_glob() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	fs::write(dir.path().join("secret.key"), b"top secret").unwrap();
	fs::write(dir.path().join(".dockerignore"), b"*.key\n").unwrap();
	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();

	// Decompress and scan for secret.key in tar entry names.
	let mut gz_content = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut gz_content)
		.unwrap();
	let mut archive = tar::Archive::new(gz_content.as_slice());
	let names: Vec<String> = archive
		.entries()
		.unwrap()
		.filter_map(|e| e.ok())
		.filter_map(|e| e.path().ok().map(|p| p.to_string_lossy().into_owned()))
		.collect();
	assert!(
		!names.iter().any(|n| n.contains("secret.key")),
		"secret.key must be excluded: {names:?}"
	);
}

#[test]
fn context_tar_with_subdirectory() {
	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	fs::create_dir(dir.path().join("src")).unwrap();
	fs::write(dir.path().join("src/main.rs"), b"fn main() {}").unwrap();
	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();
	assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
}

// build_context_tar_with_inline ----------------------------------------

#[test]
fn inline_tar_produces_valid_gzip() {
	let dir = tempdir().unwrap();
	fs::write(dir.path().join("app.txt"), b"content").unwrap();
	let inline = "FROM alpine\nRUN echo hello\n";
	let (bytes, df_name) = build_context_tar_with_inline(dir.path(), inline, &[]).unwrap();
	assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
	assert!(!df_name.is_empty());
}

#[test]
fn inline_dockerfile_excluded_via_dockerignore() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("app.txt"), b"x").unwrap();
	let (bytes, _) = build_context_tar_with_inline(dir.path(), "FROM alpine\n", &[]).unwrap();

	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	let mut di = String::new();
	for entry in archive.entries().unwrap() {
		let mut entry = entry.unwrap();
		// No user ignore file in this context, so the synthesized one is
		// written as `.containerignore`, the name podman prefers, so a
		// stray `.dockerignore` cannot shadow our exclusions.
		if entry.path().unwrap().to_string_lossy() == ".containerignore" {
			entry.read_to_string(&mut di).unwrap();
		}
	}
	assert!(
		di.lines().any(|l| l == ".dockerfile-inline"),
		"inline Dockerfile must be excluded from COPY via .dockerignore: {di:?}"
	);
}

#[test]
fn inline_dockerignore_preserves_user_rules() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("app.txt"), b"x").unwrap();
	fs::write(dir.path().join(".dockerignore"), b"*.log\n").unwrap();
	let (bytes, _) = build_context_tar_with_inline(dir.path(), "FROM alpine\n", &[]).unwrap();

	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	// The user's `.dockerignore` must appear exactly once, merged with the
	// synthesized inline-Dockerfile exclusion.
	let mut di_entries = 0;
	let mut di = String::new();
	for entry in archive.entries().unwrap() {
		let mut entry = entry.unwrap();
		if entry.path().unwrap().to_string_lossy() == ".dockerignore" {
			di_entries += 1;
			entry.read_to_string(&mut di).unwrap();
		}
	}
	assert_eq!(di_entries, 1, "exactly one .dockerignore entry");
	assert!(di.lines().any(|l| l == "*.log"), "user rule kept: {di:?}");
	assert!(
		di.lines().any(|l| l == ".dockerfile-inline"),
		"inline exclusion added: {di:?}"
	);
}

#[cfg(unix)]
#[test]
fn context_tar_packs_symlink_as_link_not_target() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	// A symlink that points outside the context (e.g. to a host secret) must be
	// stored as a link, never dereferenced into the image.
	let outside = tempdir().unwrap();
	fs::write(outside.path().join("secret"), b"TOPSECRET").unwrap();

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	std::os::unix::fs::symlink(outside.path().join("secret"), dir.path().join("leak")).unwrap();

	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();
	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());

	let mut found = false;
	for entry in archive.entries().unwrap() {
		let entry = entry.unwrap();
		if entry.path().unwrap().to_string_lossy() == "leak" {
			found = true;
			assert_eq!(
				entry.header().entry_type(),
				tar::EntryType::Symlink,
				"symlink must be packed as a link"
			);
			assert_eq!(
				entry.header().size().unwrap(),
				0,
				"link entry must not carry the target's bytes"
			);
		}
	}
	assert!(found, "symlink entry must be present in the context tar");
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

/// #1096: with both ignore files present, only `.containerignore` may filter the
/// context tar. Applying both client-side is what made podup ship an image
/// missing content that `podman build` includes: we dropped `b.txt` per
/// `.dockerignore` (a file podman ignores entirely here) and the server then
/// dropped `a.txt` per `.containerignore`, so the union of both applied.
#[test]
fn context_tar_filters_by_containerignore_only_when_both_exist() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	fs::write(dir.path().join("a.txt"), b"A").unwrap();
	fs::write(dir.path().join("b.txt"), b"B").unwrap();
	fs::write(dir.path().join(".containerignore"), b"a.txt\n").unwrap();
	fs::write(dir.path().join(".dockerignore"), b"b.txt\n").unwrap();

	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();
	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	let names: Vec<String> = archive
		.entries()
		.unwrap()
		.map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
		.collect();

	assert!(
		!names.iter().any(|n| n == "a.txt"),
		"`.containerignore` must exclude a.txt: {names:?}"
	);
	assert!(
		names.iter().any(|n| n == "b.txt"),
		"`.dockerignore` must NOT apply when `.containerignore` exists: {names:?}"
	);
}

/// The mirror: with only `.dockerignore` present it is the one that applies, so
/// existing projects that never had a `.containerignore` are unaffected.
#[test]
fn context_tar_falls_back_to_dockerignore_when_alone() {
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	fs::write(dir.path().join("b.txt"), b"B").unwrap();
	fs::write(dir.path().join(".dockerignore"), b"b.txt\n").unwrap();

	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();
	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	let names: Vec<String> = archive
		.entries()
		.unwrap()
		.map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
		.collect();
	assert!(
		!names.iter().any(|n| n == "b.txt"),
		"`.dockerignore` must still apply when it is the only one: {names:?}"
	);
}

#[test]
fn a_negated_file_survives_under_an_ignored_directory() {
	// Pruning an ignored directory is only sound when nothing under it can
	// be re-included. `.dockerignore` negation does exactly that, and the
	// pruning that landed for the enumeration cost dropped `keep.txt` from
	// the context, so a `COPY vendor/keep.txt` lost its source.
	//
	// The walk's own comment justified the shortcut by asserting the engine
	// had no such patterns in the wild. That is an assertion about what
	// users write, and the format supports negation so they can.
	use flate2::read::GzDecoder;
	use std::io::Read;

	let dir = tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	fs::write(
		dir.path().join(".dockerignore"),
		b"vendor/\n!vendor/keep.txt\n",
	)
	.unwrap();
	fs::create_dir(dir.path().join("vendor")).unwrap();
	fs::write(dir.path().join("vendor/keep.txt"), b"keep me").unwrap();
	fs::write(dir.path().join("vendor/drop.txt"), b"drop me").unwrap();

	let bytes = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();
	let mut raw = Vec::new();
	GzDecoder::new(bytes.as_slice())
		.read_to_end(&mut raw)
		.unwrap();
	let mut archive = tar::Archive::new(raw.as_slice());
	let names: Vec<String> = archive
		.entries()
		.unwrap()
		.map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
		.collect();

	assert!(
		names.iter().any(|n| n.ends_with("vendor/keep.txt")),
		"the re-included file must survive the prune, got: {names:?}"
	);
	assert!(
		!names.iter().any(|n| n.ends_with("vendor/drop.txt")),
		"the rest of the ignored directory must still be dropped, got: {names:?}"
	);
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
