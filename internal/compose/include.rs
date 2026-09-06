//! `include:` directive: merging externally included compose files.
//!
//! Included files are merged into the parent: services, volumes, networks,
//! secrets, configs, and models from the included file are added only if the
//! key does not already exist in the parent (parent wins on conflict).

use std::path::Path;

use super::types::ComposeFile;
use crate::error::{ComposeError, Result};

/// Read and parse a file referenced by `include:`, wrapping every failure in
/// [`ComposeError::Include`] so a consumer matching on the variant can tell the
/// failure originated from an included file rather than the top-level compose
/// file. A missing included file is not the same as a missing main file, and
/// an invalid-YAML included file is not the same as a malformed main file:
/// the variant lets a handler branch on the difference, and the message names
/// the included path so the operator can find it.
pub(super) fn parse_included_file(
	path: &Path,
	dir: &Path,
	env_files: &[String],
	interpolate: bool,
) -> Result<ComposeFile> {
	let display = path.display().to_string();
	super::parse_file_inner_with_env(path, dir, env_files, interpolate).map_err(|e| match e {
		ComposeError::FileNotFound(p) => {
			ComposeError::Include(format!("included compose file not found: {p}"))
		}
		ComposeError::Parse(yaml_err) => {
			let msg = match yaml_err.location() {
				Some(loc) => format!(
					"failed to parse included compose file '{display}' at line {}, column {}",
					loc.line(),
					loc.column()
				),
				None => format!("failed to parse included compose file '{display}'"),
			};
			ComposeError::Include(msg)
		}
		ComposeError::Io(io_err) => ComposeError::Include(format!(
			"io error reading included compose file '{display}': {io_err}"
		)),
		other => ComposeError::Include(format!("included compose file '{display}': {other}")),
	})
}

/// Merge `other` into `target`.
///
/// Services / volumes / networks / secrets / configs / models from `other` are
/// added; existing entries in `target` win on conflict (parent file overrides
/// included content).
pub(super) fn merge_compose_file(target: &mut ComposeFile, other: ComposeFile) {
	for (k, v) in other.services {
		target.services.entry(k).or_insert(v);
	}
	for (k, v) in other.volumes {
		target.volumes.entry(k).or_insert(v);
	}
	for (k, v) in other.networks {
		target.networks.entry(k).or_insert(v);
	}
	for (k, v) in other.secrets {
		target.secrets.entry(k).or_insert(v);
	}
	for (k, v) in other.configs {
		target.configs.entry(k).or_insert(v);
	}
	for (k, v) in other.models {
		target.models.entry(k).or_insert(v);
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "include_tests.rs"]
mod tests;
