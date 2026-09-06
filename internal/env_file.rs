//! `env_file:` loading for services.
//!
//! Reads KEY=VALUE pairs from files listed in a service's `env_file:` field.
//! Service-level `environment:` takes precedence over `env_file:` values.

use std::collections::HashMap;
use std::path::Path;

use crate::compose::types::EnvFileEntry;
use crate::error::{ComposeError, Result};

/// Load all `env_file` paths relative to `base_dir`.
///
/// Returns a merged map.  If the same key appears in multiple files, the
/// last file wins (later entries in the list override earlier ones).
/// `env_file:` never overrides service-level `environment:`.
///
/// Each file is parsed with dotenv rules (quote stripping, escapes, inline
/// comments, multi-line quoted values).
///
/// Returns [`ComposeError::FileNotFound`] when an env file does not exist.
pub fn load_env_files(paths: &[String], base_dir: &Path) -> Result<HashMap<String, String>> {
	let entries: Vec<EnvFileEntry> = paths
		.iter()
		.map(|p| EnvFileEntry::Path(p.clone()))
		.collect();
	load_env_file_entries(&entries, base_dir)
}

/// Load env_file entries supporting both short and long-form (with `required` and `format`).
///
/// When `required: false`, a missing file is silently skipped instead of returning an error.
pub fn load_env_file_entries(
	entries: &[EnvFileEntry],
	base_dir: &Path,
) -> Result<HashMap<String, String>> {
	let mut result: HashMap<String, String> = HashMap::new();

	for entry in entries {
		if let EnvFileEntry::Config {
			format: Some(fmt), ..
		} = entry
		{
			if fmt != "dotenv" {
				// compose-go logs a warning and falls back to dotenv parsing
				// rather than failing the file; match that lenient behaviour.
				tracing::warn!(
					"env_file format '{fmt}' is not supported; parsing '{}' as dotenv",
					entry.path()
				);
			}
		}

		let abs = base_dir.join(entry.path());
		let content = match crate::filesystem::read_to_string_capped(&abs) {
			Ok(c) => c,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
				if entry.required() {
					return Err(ComposeError::FileNotFound(abs.display().to_string()));
				} else {
					continue;
				}
			}
			Err(e) => return Err(ComposeError::Io(e)),
		};

		// A service `env_file:` is explicitly requested, so a malformed entry
		// (e.g. an unterminated quoted value that would otherwise swallow the
		// following keys) is a hard error rather than silent data loss.
		for (key, value) in crate::dotenv::parse_strict(&content)? {
			result.insert(key, value);
		}
	}

	Ok(result)
}

/// Merge env_file values with service environment.
///
/// `service_env` takes precedence: only keys not already in `service_env` are added.
pub fn merge_env(
	service_env: HashMap<String, Option<String>>,
	env_file_vars: HashMap<String, String>,
) -> Vec<String> {
	let mut merged = service_env;
	for (k, v) in env_file_vars {
		merged.entry(k).or_insert(Some(v));
	}

	merged
		.into_iter()
		.map(|(k, v)| match v {
			Some(val) => format!("{k}={val}"),
			None => k,
		})
		.collect()
}

/// Fold every service's `env_file:` into its `environment:` and drop the key.
///
/// `config` is meant to render the canonical, fully-resolved model, and docker
/// compose materialises `env_file` there. Leaving it unresolved meant a service
/// that takes its whole environment from a file rendered with no `environment:`
/// at all, so the one command you reach for to ask "what will this actually run"
/// pointed away from the answer rather than merely omitting it (#1184).
///
/// Precedence is the same as at run time: `environment:` wins over `env_file:`,
/// and a later file wins over an earlier one. Keys are emitted sorted so the
/// output is stable across runs rather than following a hash map's order.
///
/// A bare `KEY` (inherit from the host) stays valueless, rendering as `KEY: null`
/// the way the parser accepts it back.
pub fn materialize_env_files(
	file: &mut crate::compose::types::ComposeFile,
	base_dir: &Path,
) -> Result<()> {
	for service in file.services.values_mut() {
		let entries = service.env_file.to_entries();
		if entries.is_empty() {
			continue;
		}
		let from_files = load_env_file_entries(&entries, base_dir)?;
		let mut merged = service.environment.to_map();
		for (k, v) in from_files {
			merged.entry(k).or_insert(Some(v));
		}
		let mut keys: Vec<String> = merged.keys().cloned().collect();
		keys.sort();
		let map = keys
			.into_iter()
			.map(|k| {
				let value = merged
					.get(&k)
					.and_then(|v| v.clone())
					.map(serde_yaml::Value::String);
				(k, value)
			})
			.collect();
		service.environment = crate::compose::types::EnvVars::Map(map);
		service.env_file = crate::compose::types::EnvFile::Empty;
	}
	Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "env_file_tests.rs"]
mod tests;
