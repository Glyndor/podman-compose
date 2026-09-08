//! `extends:` directive: inheritance and field merging between service definitions.
//!
//! Services can extend another service within the same file or from an external
//! compose file referenced by path. Resolution is recursive (chains are supported)
//! and cycle detection uses a visited set to error early.
//!
//! Merge semantics: scalar fields from the child win; collection fields
//! (env vars, labels, vectors) are merged with the child taking precedence on
//! overlapping keys. See [`merge_service`] for full field-by-field rules.

mod merge;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::parse_file_inner;
use super::types::ComposeFile;
use crate::error::{ComposeError, Result};

pub(in crate::compose) use merge::{merge_service, merge_service_tagged};

const MAX_EXTENDS_DEPTH: usize = 16;

/// How many distinct external `extends.file` paths the cache may keep
/// before `resolve_all_extends` starts refusing. Bounded so a project
/// that points every service at a different file cannot grow the cache
/// without limit; the cap is high enough that the only realistic way
/// to hit it is to be the shape the issue describes (`extends.file` per
/// service, all pointing at the same file), at which point the cache
/// either reuses the single entry or refuses with a clear message.
const MAX_EXTENDS_CACHE_ENTRIES: usize = 256;

// Thread-local counter used by the unit tests to assert that
// `parse_file_inner` is not re-invoked per referencing service when
// `extends.file` points at a shared file. Without caching, the
// counter records one increment per referencing service; with
// caching, exactly one regardless of the number of references.
// Compiled out of release builds.
#[cfg(test)]
std::thread_local! {
	static PARSE_FILE_INNER_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_parse_file_inner_counter() {
	PARSE_FILE_INNER_CALLS.with(|c| c.set(0));
}

#[cfg(test)]
fn parse_file_inner_call_count() -> u32 {
	PARSE_FILE_INNER_CALLS.with(|c| c.get())
}

#[cfg(test)]
fn bump_parse_file_inner_counter() {
	PARSE_FILE_INNER_CALLS.with(|c| c.set(c.get() + 1));
}

/// Cache of resolved external files, keyed by their canonicalized
/// absolute path. The first referencing service populates the entry;
/// every subsequent one reuses the same parsed `ComposeFile` instead
/// of re-reading and re-parsing the same file from disk.
///
/// Without this cache, a project that has twenty services each saying
/// `extends: { service: base, file: common.yml }` reads and parses
/// `common.yml` twenty times, with each parse holding the YAML value
/// tree in memory while the merge runs. At the 16 MiB per-file cap
/// that already exists, twenty concurrent parses peak at 5.8 GB and
/// the process aborts in seconds (#1746).
///
/// Caching turns that into one parse, plus a clone per referencing
/// service so the per-service `swap_remove` does not corrupt the
/// shared entry. The clone of a parsed `ComposeFile` is dominated by
/// the file size; at 16 MiB twenty clones is well under what twenty
/// concurrent parses needed.
#[derive(Default)]
struct ExtendsCache {
	entries: HashMap<PathBuf, ComposeFile>,
	/// Paths currently being resolved. `entries` is populated only after a
	/// file's own chains resolve, so it cannot answer "is this path on the
	/// current path" and a cycle across files would recurse forever.
	in_progress: HashSet<PathBuf>,
}

impl ExtendsCache {
	fn get_or_load(&mut self, abs: &Path, base_dir: &Path, depth: usize) -> Result<ComposeFile> {
		if let Some(cached) = self.entries.get(abs) {
			return Ok(cached.clone());
		}
		// An entry is inserted only after its own `extends:` chains resolve,
		// so a file that is still being resolved is absent from `entries`
		// and a cycle across files would recurse until the stack ran out.
		// `in_progress` is the marker the cache alone cannot provide: it
		// says "this path is on the current resolution path", which is what
		// the visited set does for in-file chains.
		if !self.in_progress.insert(abs.to_path_buf()) {
			return Err(ComposeError::Extends(format!(
				"circular extends.file: {} is already being resolved",
				abs.display()
			)));
		}
		#[cfg(test)]
		bump_parse_file_inner_counter();
		let dir = abs
			.parent()
			.map(|p| p.to_path_buf())
			.unwrap_or_else(|| base_dir.to_path_buf());
		let other = parse_file_inner(abs, &dir)?;
		// Resolve the external file's own `extends:` chains before
		// caching, so a later `swap_remove` of the requested base
		// service gives a fully-merged value. The cached entry is the
		// resolved file; per-service callers still do their own merges
		// of the base into the child.
		// The cache holds the PARSED file, not a fully resolved one. It used
		// to resolve every service here before caching, which made a
		// reference to one valid service fail when an unrelated service in
		// the same file extended something missing: a file you do not
		// control could break your build over a service you never asked
		// for. The caller resolves the chain it actually needs, immediately
		// after this returns, so nothing is lost by not doing it here.
		//
		// The parse is what the cache exists to save (#1746): twenty
		// services pointing at one file parsed it twenty times and peaked
		// at 5.8 GB. That saving is unaffected.
		//
		// `in_progress` and the depth counter still work, because the
		// caller's `resolve_one_extends` recurses back through this
		// function for any nested `extends.file`.
		let _ = depth;
		if self.entries.len() >= MAX_EXTENDS_CACHE_ENTRIES && !self.entries.contains_key(abs) {
			self.in_progress.remove(abs);
			return Err(ComposeError::Extends(format!(
				"extends.file cache exceeded {MAX_EXTENDS_CACHE_ENTRIES} distinct files; \
				 refactor the project so fewer services point at distinct external files"
			)));
		}
		self.in_progress.remove(abs);
		let cloned = other.clone();
		self.entries.insert(abs.to_path_buf(), other);
		Ok(cloned)
	}
}

/// Resolve `extends:` only within the same file (no `file:` references).
///
/// Used by [`super::parse_str`] where there is no on-disk path.
pub(super) fn resolve_extends_same_file(file: &mut ComposeFile) -> Result<()> {
	let names: Vec<String> = file.services.keys().cloned().collect();
	for name in names {
		let mut visited: HashSet<String> = HashSet::new();
		resolve_one_extends_in_memory(file, &name, &mut visited, 0)?;
	}
	Ok(())
}

/// Resolve `extends:` for every service in `file`, including chains across
/// other compose files referenced by `extends.file`. External files are
/// parsed at most once each, even when many services reference the same
/// path; see [`ExtendsCache`].
pub(super) fn resolve_all_extends(file: &mut ComposeFile, base_dir: &Path) -> Result<()> {
	let mut cache = ExtendsCache::default();
	resolve_all_extends_with_cache(file, base_dir, &mut cache, 0)
}

fn resolve_all_extends_with_cache(
	file: &mut ComposeFile,
	base_dir: &Path,
	cache: &mut ExtendsCache,
	depth: usize,
) -> Result<()> {
	let names: Vec<String> = file.services.keys().cloned().collect();
	for name in names {
		let mut visited: HashSet<String> = HashSet::new();
		resolve_one_extends(file, &name, base_dir, &mut visited, depth, cache)?;
	}
	Ok(())
}

fn resolve_one_extends_in_memory(
	file: &mut ComposeFile,
	name: &str,
	visited: &mut HashSet<String>,
	depth: usize,
) -> Result<()> {
	if depth >= MAX_EXTENDS_DEPTH {
		return Err(ComposeError::Extends(format!(
			"extends chain exceeds maximum depth ({MAX_EXTENDS_DEPTH}) at service '{name}'"
		)));
	}
	if !visited.insert(name.to_string()) {
		return Err(ComposeError::Extends(format!("circular extends at {name}")));
	}

	let extends = match file.services.get(name).and_then(|s| s.extends.clone()) {
		Some(e) => e,
		None => return Ok(()),
	};

	if extends.file().is_some() {
		return Err(ComposeError::Extends(format!(
			"service '{name}' uses 'extends.file' but parser was given a string, not a path"
		)));
	}

	let base_name = extends.service().to_string();
	if base_name == name {
		return Err(ComposeError::Extends(format!(
			"service '{name}' extends itself"
		)));
	}

	if file.services.get(&base_name).is_none() {
		return Err(ComposeError::Extends(format!(
			"service '{name}' extends unknown service '{base_name}'"
		)));
	}
	resolve_one_extends_in_memory(file, &base_name, visited, depth + 1)?;

	let base = file
		.services
		.get(&base_name)
		.cloned()
		.ok_or_else(|| ComposeError::Extends(base_name.clone()))?;

	if let Some(svc) = file.services.get_mut(name) {
		let merged = merge_service(base, svc.clone());
		*svc = merged;
		svc.extends = None;
	}

	Ok(())
}

fn resolve_one_extends(
	file: &mut ComposeFile,
	name: &str,
	base_dir: &Path,
	visited: &mut HashSet<String>,
	depth: usize,
	cache: &mut ExtendsCache,
) -> Result<()> {
	if depth >= MAX_EXTENDS_DEPTH {
		return Err(ComposeError::Extends(format!(
			"extends chain exceeds maximum depth ({MAX_EXTENDS_DEPTH}) at service '{name}'"
		)));
	}
	if !visited.insert(name.to_string()) {
		return Err(ComposeError::Extends(format!("circular extends at {name}")));
	}

	let extends = match file.services.get(name).and_then(|s| s.extends.clone()) {
		Some(e) => e,
		None => return Ok(()),
	};

	let base_name = extends.service().to_string();

	let base_service = if let Some(file_path) = extends.file() {
		// The compose file is trusted input (like a Makefile): `extends.file`
		// may use `../` or absolute paths, matching docker-compose and
		// podman-compose. Do not confine it.
		let abs = base_dir.join(file_path);
		let abs = abs.canonicalize().unwrap_or(abs);
		// `get_or_load` reads and parses the external file at most once per
		// path across the whole `resolve_all_extends` call: the first
		// referencing service populates the cache, every subsequent
		// service gets a clone. Without the cache, twenty services
		// pointing at the same file would parse it twenty times and
		// peak at 5.8 GB before the process aborts (#1746).
		let mut other = cache.get_or_load(&abs, base_dir, depth + 1)?;
		let ext_dir = abs.parent().unwrap_or(base_dir);
		let mut nested_visited: HashSet<String> = HashSet::new();
		resolve_one_extends(
			&mut other,
			&base_name,
			ext_dir,
			&mut nested_visited,
			depth + 1,
			cache,
		)?;
		let mut base = other.services.swap_remove(&base_name).ok_or_else(|| {
			ComposeError::Extends(format!(
				"service '{base_name}' not found in {}",
				abs.display()
			))
		})?;
		// The base service's relative paths are relative to the external file's
		// directory; anchor them before merging into the current file's service.
		super::anchor::anchor_service(&mut base, ext_dir);
		base
	} else {
		if base_name == name {
			return Err(ComposeError::Extends(format!(
				"service '{name}' extends itself"
			)));
		}
		if !file.services.contains_key(&base_name) {
			return Err(ComposeError::Extends(format!(
				"service '{name}' extends unknown service '{base_name}'"
			)));
		}
		resolve_one_extends(file, &base_name, base_dir, visited, depth + 1, cache)?;
		file.services
			.get(&base_name)
			.cloned()
			.ok_or_else(|| ComposeError::Extends(base_name.clone()))?
	};

	if let Some(svc) = file.services.get_mut(name) {
		let merged = merge_service(base_service, svc.clone());
		*svc = merged;
		svc.extends = None;
	}

	Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "parse_count_tests.rs"]
mod parse_count_tests;
#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
