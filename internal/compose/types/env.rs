//! Environment variable and env-file types for the `environment:` and `env_file:` service fields.
//!
//! [`EnvVars`] accepts list or map form. [`EnvFile`] accepts a single path, a list of paths,
//! or a list of long-form [`EnvFileEntry`] objects with optional `required:` and `format:` fields.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Environment variables as a list (`["KEY=VAL"]`) or map (`{KEY: VAL}`).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum EnvVars {
	/// The field was absent.
	#[default]
	Empty,
	/// List form: `KEY=VAL` or bare `KEY` entries.
	List(Vec<String>),
	/// Map form: keys to optional values (a null value inherits from the host).
	Map(IndexMap<String, Option<serde_yaml::Value>>),
}

impl EnvVars {
	/// Returns the variables as a map, where `None` means inherit from the host.
	pub fn to_map(&self) -> HashMap<String, Option<String>> {
		match self {
			EnvVars::Empty => HashMap::new(),
			EnvVars::List(list) => list
				.iter()
				.filter_map(|s| {
					let mut parts = s.splitn(2, '=');
					let key = parts.next()?.to_string();
					let val = parts.next().map(|v| v.to_string());
					Some((key, val))
				})
				.collect(),
			EnvVars::Map(map) => map
				.iter()
				.map(|(k, v)| {
					let val = v.as_ref().and_then(|v| match v {
						serde_yaml::Value::String(s) => Some(s.clone()),
						serde_yaml::Value::Number(n) => Some(n.to_string()),
						serde_yaml::Value::Bool(b) => Some(b.to_string()),
						serde_yaml::Value::Null => None,
						_ => None,
					});
					(k.clone(), val)
				})
				.collect(),
		}
	}

	/// Returns whether no environment variables are defined.
	pub fn is_empty(&self) -> bool {
		match self {
			EnvVars::Empty => true,
			EnvVars::List(v) => v.is_empty(),
			EnvVars::Map(m) => m.is_empty(),
		}
	}
}

/// One entry in an `env_file:` list — either a bare path or a long-form object.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum EnvFileEntry {
	/// Short form: a bare env file path.
	Path(String),
	/// Long form: an env file path with `required`/`format` options.
	Config {
		/// Path to the env file.
		path: String,
		/// Whether a missing file is an error; `true` if absent.
		#[serde(skip_serializing_if = "Option::is_none")]
		required: Option<bool>,
		/// Env file format selector (e.g. `raw`).
		#[serde(skip_serializing_if = "Option::is_none")]
		format: Option<String>,
	},
}

impl EnvFileEntry {
	/// Returns the env file path.
	pub fn path(&self) -> &str {
		match self {
			EnvFileEntry::Path(p) => p,
			EnvFileEntry::Config { path, .. } => path,
		}
	}

	/// `true` by default — missing file is an error unless `required: false`.
	pub fn required(&self) -> bool {
		match self {
			EnvFileEntry::Path(_) => true,
			EnvFileEntry::Config { required, .. } => required.unwrap_or(true),
		}
	}
}

/// `env_file:` field — single path, list of paths, or list of long-form objects.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum EnvFile {
	/// The field was absent.
	#[default]
	Empty,
	/// A single env file entry.
	Single(EnvFileEntry),
	/// A list of env file entries.
	List(Vec<EnvFileEntry>),
}

impl EnvFile {
	/// Returns all env file entries.
	pub fn to_entries(&self) -> Vec<EnvFileEntry> {
		match self {
			EnvFile::Empty => vec![],
			EnvFile::Single(e) => vec![e.clone()],
			EnvFile::List(v) => v.clone(),
		}
	}

	/// Return just the paths (strips `required` / `format` info).
	pub fn to_list(&self) -> Vec<String> {
		self.to_entries()
			.into_iter()
			.map(|e| e.path().to_string())
			.collect()
	}

	/// Returns whether no env files are defined.
	pub fn is_empty(&self) -> bool {
		match self {
			EnvFile::Empty => true,
			EnvFile::Single(_) => false,
			EnvFile::List(v) => v.is_empty(),
		}
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "env_tests.rs"]
mod tests;
