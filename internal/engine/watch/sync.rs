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
#[path = "sync_tests.rs"]
mod tests;
