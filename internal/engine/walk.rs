//! Filesystem walking helpers used by the engine to discover build contexts
//! and other recursively-resolved paths.
//!
//! Kept in its own module so `engine::mod` stays within the 500-line hard cap
//! that the org's `line-limit` reusable enforces. Pure and total so the
//! recursion is unit-testable without standing up a real build context.

use std::path::PathBuf;

/// Recursive walk rooted at `root`, returning every entry (file or directory)
/// in deterministic path order. Pure helper; used by the build-context
/// materialisation paths to stage a tree before `podman build`.
pub(in crate::engine) fn walk_dir(root: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
	let mut out = Vec::new();
	walk_collect(root, &mut out)?;
	Ok(out)
}

fn walk_collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
	let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
	entries.sort_by_key(|e| e.file_name());
	for entry in entries {
		let path = entry.path();
		let file_type = entry.file_type()?;
		out.push(path.clone());
		if file_type.is_dir() {
			walk_collect(&path, out)?;
		}
	}
	Ok(())
}

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
