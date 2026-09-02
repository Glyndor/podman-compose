//! `extends:` directive — inheritance and field merging between service definitions.
//!
//! Services can extend another service within the same file or from an external
//! compose file referenced by path. Resolution is recursive (chains are supported)
//! and cycle detection uses a visited set to error early.
//!
//! Merge semantics: scalar fields from the child win; collection fields
//! (env vars, labels, vectors) are merged with the child taking precedence on
//! overlapping keys. See [`merge_service`] for full field-by-field rules.

mod merge;

use std::collections::HashSet;
use std::path::Path;

use super::parse_file_inner;
use super::types::ComposeFile;
use crate::error::{ComposeError, Result};

pub(in crate::compose) use merge::{merge_service, merge_service_tagged};

const MAX_EXTENDS_DEPTH: usize = 16;

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
/// other compose files referenced by `extends.file`.
pub(super) fn resolve_all_extends(file: &mut ComposeFile, base_dir: &Path) -> Result<()> {
	let names: Vec<String> = file.services.keys().cloned().collect();
	for name in names {
		let mut visited: HashSet<String> = HashSet::new();
		resolve_one_extends(file, &name, base_dir, &mut visited, 0)?;
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
		let dir = abs
			.parent()
			.map(|p| p.to_path_buf())
			.unwrap_or_else(|| base_dir.to_path_buf());
		let mut other = parse_file_inner(&abs, &dir)?;
		let mut nested_visited: HashSet<String> = HashSet::new();
		resolve_one_extends(&mut other, &base_name, &dir, &mut nested_visited, depth + 1)?;
		let mut base = other.services.swap_remove(&base_name).ok_or_else(|| {
			ComposeError::Extends(format!(
				"service '{base_name}' not found in {}",
				abs.display()
			))
		})?;
		// The base service's relative paths are relative to the external file's
		// directory; anchor them before merging into the current file's service.
		super::anchor::anchor_service(&mut base, &dir);
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
		resolve_one_extends(file, &base_name, base_dir, visited, depth + 1)?;
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
#[path = "mod_tests.rs"]
mod tests;
