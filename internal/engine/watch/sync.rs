//! Sync-tar assembly and include/ignore filtering for watch rules.
//!
//! [`build_sync_tar`] packs a changed file or directory into a gzipped tar,
//! storing each entry under a caller-supplied archive name so the container-side
//! layout matches docker-compose `watch` (the changed path under the rule
//! `target`, subdirectories preserved). [`is_ignored`] / [`is_included`]
//! implement the `develop.watch` rule path filters.

use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::engine::walk;
use crate::error::{ComposeError, Result};

/// Pack `src` into a gzipped tar, storing its top-level entry under
/// `entry_name`.
///
/// `entry_name` is the archive path the changed file or directory should occupy
/// once extracted at the PUT destination. For a single changed file this is the
/// file's path relative to the watch-rule root (subdirectories preserved), or
/// the rename target's basename when the rule watches a single file. For a
/// directory `src`, every walked descendant is stored under `entry_name`,
/// preserving the in-tree layout.
pub(super) fn build_sync_tar(src: &Path, entry_name: &Path) -> Result<Vec<u8>> {
	let encoder = GzEncoder::new(Vec::new(), Compression::default());
	let mut tar = tar::Builder::new(encoder);
	// Do not dereference symlinks: a symlink inside the watched tree would
	// otherwise copy the contents of its (possibly out-of-tree) target into the
	// container. Store the link itself instead.
	tar.follow_symlinks(false);

	if src.is_dir() {
		for abs in walk::walk_dir(src).map_err(ComposeError::Io)? {
			let rel = abs
				.strip_prefix(src)
				.map_err(|_| ComposeError::Build("path strip".into()))?;
			// Re-root each descendant under `entry_name` so the directory lands at
			// the rule target with its in-tree layout preserved.
			let name = entry_name.join(rel);
			// Classify without following symlinks so a symlink-to-dir is stored as
			// a link, not dereferenced.
			let is_dir = abs.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false);
			if is_dir {
				tar.append_dir(&name, &abs)
					.map_err(|e| ComposeError::Build(e.to_string()))?;
			} else {
				tar.append_path_with_name(&abs, &name)
					.map_err(|e| ComposeError::Build(e.to_string()))?;
			}
		}
	} else {
		tar.append_path_with_name(src, entry_name)
			.map_err(|e| ComposeError::Build(e.to_string()))?;
	}

	let gz = tar
		.into_inner()
		.map_err(|e| ComposeError::Build(e.to_string()))?;
	let bytes = gz
		.finish()
		.map_err(|e| ComposeError::Build(e.to_string()))?;
	Ok(bytes)
}

/// True when `path` matches a watch-rule `ignore` pattern. A pattern ending in
/// `/` matches `path` by directory prefix; otherwise it matches an exact path or
/// a leading path segment (the pattern followed by `/`). Matching is anchored at
/// the start of `path`.
pub(super) fn is_ignored(path: &str, patterns: &[String]) -> bool {
	for pat in patterns {
		if pat.ends_with('/') {
			if path.starts_with(pat.as_str()) {
				return true;
			}
		} else if path == pat.as_str()
			|| (path.starts_with(pat.as_str()) && path.as_bytes().get(pat.len()) == Some(&b'/'))
		{
			return true;
		}
	}
	false
}

/// True when `path` matches a watch-rule `include` pattern. Unlike
/// [`is_ignored`], a `*.ext` pattern matches by extension suffix, and a bare name
/// matches not only an exact path or directory prefix but also a trailing path
/// segment anywhere in `path` (the pattern preceded by `/`).
pub(super) fn is_included(path: &str, patterns: &[String]) -> bool {
	for pat in patterns {
		if pat.starts_with("*.") {
			let ext = &pat[1..];
			if path.ends_with(ext) {
				return true;
			}
		} else if pat.ends_with('/') {
			if path.starts_with(pat.as_str()) {
				return true;
			}
		} else if path == pat.as_str()
			|| (path.len() > pat.len() + 1
				&& path.as_bytes()[path.len() - pat.len() - 1] == b'/'
				&& path.ends_with(pat.as_str()))
		{
			return true;
		}
	}
	false
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::{build_sync_tar, is_ignored, is_included};
	use flate2::read::GzDecoder;
	use std::fs;
	use std::io::Read;
	use std::path::Path;
	use tempfile::tempdir;

	fn pats(v: &[&str]) -> Vec<String> {
		v.iter().map(|s| s.to_string()).collect()
	}

	/// Decode a gzipped tar and collect its non-directory entry paths.
	fn tar_entry_paths(gz: &[u8]) -> Vec<String> {
		let mut decoder = GzDecoder::new(gz);
		let mut raw = Vec::new();
		decoder.read_to_end(&mut raw).unwrap();
		let mut archive = tar::Archive::new(&raw[..]);
		let mut names = Vec::new();
		for entry in archive.entries().unwrap() {
			let entry = entry.unwrap();
			if entry.header().entry_type().is_file() {
				names.push(entry.path().unwrap().to_string_lossy().replace('\\', "/"));
			}
		}
		names
	}

	// is_ignored -----------------------------------------------------------

	#[test]
	fn ignored_exact_file() {
		assert!(is_ignored("Makefile", &pats(&["Makefile"])));
	}

	#[test]
	fn ignored_not_prefix_match() {
		assert!(!is_ignored("Makefile.local", &pats(&["Makefile"])));
	}

	#[test]
	fn ignored_dir_prefix() {
		assert!(is_ignored("node_modules/foo.js", &pats(&["node_modules/"])));
	}

	#[test]
	fn ignored_dir_prefix_no_partial() {
		assert!(!is_ignored("nonode_modules/foo", &pats(&["node_modules/"])));
	}

	#[test]
	fn ignored_path_with_slash() {
		assert!(is_ignored("vendor/lib.rs", &pats(&["vendor"])));
	}

	#[test]
	fn ignored_empty_patterns() {
		assert!(!is_ignored("anything.rs", &[]));
	}

	#[test]
	fn ignored_no_match() {
		assert!(!is_ignored("src/main.rs", &pats(&["target/", "*.log"])));
	}

	// is_included ----------------------------------------------------------

	#[test]
	fn included_glob_extension() {
		assert!(is_included("src/main.rs", &pats(&["*.rs"])));
	}

	#[test]
	fn included_glob_no_match() {
		assert!(!is_included("src/main.go", &pats(&["*.rs"])));
	}

	#[test]
	fn included_dir_prefix() {
		assert!(is_included("src/main.rs", &pats(&["src/"])));
	}

	#[test]
	fn included_dir_prefix_no_match() {
		assert!(!is_included("test/main.rs", &pats(&["src/"])));
	}

	#[test]
	fn included_exact_match() {
		assert!(is_included("Makefile", &pats(&["Makefile"])));
	}

	#[test]
	fn included_path_segment_suffix() {
		assert!(is_included("src/lib.rs", &pats(&["lib.rs"])));
	}

	#[test]
	fn included_empty_patterns() {
		assert!(!is_included("anything", &[]));
	}

	// build_sync_tar -------------------------------------------------------

	#[test]
	fn sync_tar_single_file() {
		let dir = tempdir().unwrap();
		let file = dir.path().join("hello.txt");
		fs::write(&file, b"hello world").unwrap();
		let bytes = build_sync_tar(&file, Path::new("hello.txt")).unwrap();
		// gzip magic bytes
		assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
		assert_eq!(tar_entry_paths(&bytes), vec!["hello.txt"]);
	}

	#[test]
	fn sync_tar_single_file_renamed_entry() {
		// A single-file rule whose target renames the file: the entry must be
		// stored under the supplied (target) name, not the source basename.
		let dir = tempdir().unwrap();
		let file = dir.path().join("settings.yml");
		fs::write(&file, b"k: v").unwrap();
		let bytes = build_sync_tar(&file, Path::new("config.yml")).unwrap();
		assert_eq!(tar_entry_paths(&bytes), vec!["config.yml"]);
	}

	#[test]
	fn sync_tar_subpath_entry_preserved() {
		// A directory rule where a nested file changed: re-rooting under the
		// supplied entry name must preserve the subdirectory.
		let dir = tempdir().unwrap();
		let file = dir.path().join("b.txt");
		fs::write(&file, b"file b").unwrap();
		let bytes = build_sync_tar(&file, Path::new("sub/b.txt")).unwrap();
		assert_eq!(tar_entry_paths(&bytes), vec!["sub/b.txt"]);
	}

	#[test]
	fn sync_tar_directory() {
		let dir = tempdir().unwrap();
		fs::write(dir.path().join("a.txt"), b"file a").unwrap();
		fs::create_dir(dir.path().join("sub")).unwrap();
		fs::write(dir.path().join("sub/b.txt"), b"file b").unwrap();
		let bytes = build_sync_tar(dir.path(), Path::new("dst")).unwrap();
		assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
		// Every descendant is re-rooted under the supplied entry name, layout kept.
		let mut names = tar_entry_paths(&bytes);
		names.sort();
		assert_eq!(names, vec!["dst/a.txt", "dst/sub/b.txt"]);
	}

	#[test]
	fn sync_tar_path_with_no_file_name() {
		// A path that has no file_name (e.g. root "/") — tar should be empty but valid.
		let dir = tempdir().unwrap();
		// Empty directory — no entries other than root
		let bytes = build_sync_tar(dir.path(), Path::new(".")).unwrap();
		assert_eq!(&bytes[..2], &[0x1f, 0x8b]);
	}
}
