//! Filesystem walking helpers used by the engine to discover build contexts
//! and other recursively-resolved paths.
//!
//! Kept in its own module so `engine::mod` stays within the 500-line hard cap
//! that the org's `line-limit` reusable enforces. Pure and total so the
//! recursion is unit-testable without standing up a real build context.

use std::path::{Path, PathBuf};

// Thread-local counter for `read_dir` calls inside the walk, used by the
// unit tests to assert that an ignored subtree is pruned before
// enumeration rather than walked and then filtered (#1746 entry 6).
// Compiled out of release builds.
#[cfg(test)]
std::thread_local! {
	static WALKED_PATHS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Reset the test-only walk counter. The test that pins the walk's
/// pruning behaviour calls this immediately before driving the
/// build flow, so the counter measures only the call under test and
/// not anything earlier in the process.
#[cfg(test)]
pub(in crate::engine) fn reset_walk_counter() {
	WALKED_PATHS.with(|c| c.set(0));
}

/// Read the test-only walk counter. After `reset_walk_counter`, this
/// is the number of paths the walk pushed into its output. Pre-fix
/// every entry under an ignored subtree would be pushed (one per
/// file, plus the directory itself), so the counter for a context
/// with a 200-file `target/` would be ~201 higher than the same
/// context after the fix.
#[cfg(test)]
pub(in crate::engine) fn walk_count() -> u64 {
	WALKED_PATHS.with(|c| c.get())
}

#[cfg(test)]
fn bump_walk_counter() {
	WALKED_PATHS.with(|c| c.set(c.get() + 1));
}

/// Recursive walk rooted at `root`, returning every entry (file or directory)
/// in deterministic path order. Pure helper; used by the build-context
/// materialisation paths to stage a tree before `podman build`.
pub(in crate::engine) fn walk_dir(root: &Path) -> std::io::Result<Vec<PathBuf>> {
	let mut out = Vec::new();
	walk_collect(root, &mut out)?;
	Ok(out)
}

fn walk_collect(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
	let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
	entries.sort_by_key(|e| e.file_name());
	for entry in entries {
		let path = entry.path();
		let file_type = entry.file_type()?;
		out.push(path.clone());
		#[cfg(test)]
		bump_walk_counter();
		if file_type.is_dir() {
			walk_collect(&path, out)?;
		}
	}
	Ok(())
}

/// Recursive walk that prunes any directory for which `skip_dir` returns
/// `true` before descending. The skipped directory is still recorded in
/// the output list (callers ship empty directories to the builder
/// regardless), but its contents are not enumerated. This is the shape
/// the build-context tar loop needed: `.dockerignore` patterns can drop
/// a whole `target/` or `node_modules/` subtree, and reading every file
/// under it just to filter each one out wastes a `#read_dir` plus a
/// per-entry stat per ignored entry (#1746 entry 6).
///
/// `skip_dir` is called once per directory the walk would otherwise
/// recurse into. Returning `true` means "no entry under this directory
/// will survive the filter, skip the descent"; the call site is
/// responsible for the same answer the per-file filter would give for
/// every child, so the two cannot disagree about what ends up in the
/// result. In practice the call site mirrors the existing
/// `is_ignored` check: if the directory is in a pattern, the contents
/// are too, with the negation edge case the brief calls out left for
/// the call site to handle (the engine currently has no negation
/// patterns in the wild that would re-include a child of an ignored
/// directory; if one is added the fix is to return `false` here for
/// the parent and let the leaf filter carry the negation).
pub(in crate::engine) fn walk_dir_skipping<F>(
	root: &Path,
	skip_dir: F,
) -> std::io::Result<Vec<PathBuf>>
where
	F: Fn(&Path) -> bool,
{
	let mut out = Vec::new();
	walk_collect_skipping(root, &mut out, &skip_dir)?;
	Ok(out)
}

fn walk_collect_skipping<F>(dir: &Path, out: &mut Vec<PathBuf>, skip_dir: &F) -> std::io::Result<()>
where
	F: Fn(&Path) -> bool,
{
	let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
	entries.sort_by_key(|e| e.file_name());
	for entry in entries {
		let path = entry.path();
		let file_type = entry.file_type()?;
		out.push(path.clone());
		#[cfg(test)]
		bump_walk_counter();
		if file_type.is_dir() && !skip_dir(&path) {
			walk_collect_skipping(&path, out, skip_dir)?;
		}
	}
	Ok(())
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
