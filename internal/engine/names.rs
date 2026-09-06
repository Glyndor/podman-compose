//! Client-side validation of the object names podup hands to Podman.
//!
//! Podman enforces an object-name regex (`[a-zA-Z0-9][a-zA-Z0-9_.-]*`) on
//! volumes, networks, and containers, rejecting anything else with an opaque
//! HTTP 500 ("names must match …"). [`Engine::validate_object_names`] applies
//! the same check up front, before a single create is issued, so a bad name
//! (typically an explicit `name:` on a top-level volume/network, or an explicit
//! `container_name:`) surfaces as a clear error naming the offending object,
//! and nothing is created.

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::is_valid_object_name;

use super::network::resolve_network_name;
use super::Engine;

impl Engine {
	/// Reject any volume, network, or container name that Podman's object-name
	/// regex would refuse, before issuing a single create. Validates the
	/// resolved host names podup will actually pass to the API: the
	/// project-prefixed default or the explicit `name:`/`container_name:`
	/// override. External resources are looked up, not created, so a bad
	/// external name surfaces as the clearer "does not exist" error instead.
	pub(super) fn validate_object_names(&self, file: &ComposeFile) -> Result<()> {
		validate_object_names(file, &self.project)
	}
}

/// Pure form of [`Engine::validate_object_names`], taking the project name
/// directly so it can be unit-tested without a live engine.
fn validate_object_names(file: &ComposeFile, project: &str) -> Result<()> {
	for (key, config) in &file.volumes {
		if config.as_ref().and_then(|c| c.external).unwrap_or(false) {
			continue;
		}
		let name = config
			.as_ref()
			.and_then(|c| c.name.as_deref())
			.map(str::to_string)
			.unwrap_or_else(|| format!("{project}_{key}"));
		ensure_valid_object_name("volume", key, &name)?;
	}

	for (key, config) in &file.networks {
		if config.as_ref().and_then(|c| c.external).unwrap_or(false) {
			continue;
		}
		let name = resolve_network_name(key, file, project);
		ensure_valid_object_name("network", key, &name)?;
	}

	for (svc_name, service) in &file.services {
		// Only the base name needs checking: replica suffixes (`-1`, `-2`, …)
		// append digits and a dash, which never turn a valid base invalid.
		let name = service
			.container_name
			.clone()
			.unwrap_or_else(|| format!("{project}-{svc_name}"));
		ensure_valid_object_name("container", svc_name, &name)?;
	}

	Ok(())
}

/// Error out (with a message naming the offending object) when `name` is not a
/// valid Podman object name. `kind` is the resource word ("volume", "network",
/// "container"); `key` is the compose key it derives from, mentioned only when
/// it differs from the resolved name so the user can locate the declaration.
pub(super) fn ensure_valid_object_name(kind: &str, key: &str, name: &str) -> Result<()> {
	if is_valid_object_name(name) {
		return Ok(());
	}
	let origin = if name == key {
		String::new()
	} else {
		format!(" (from {kind} '{key}')")
	};
	Err(ComposeError::Unsupported(format!(
		"invalid {kind} name {name:?}{origin}: names must start with an ASCII letter or digit \
		 and contain only letters, digits, '_', '.', '-'"
	)))
}

#[cfg(test)]
#[path = "names_tests.rs"]
mod tests;
