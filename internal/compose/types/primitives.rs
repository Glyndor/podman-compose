//! Primitive compose field types shared across multiple service keys.
//!
//! [`Command`] is a shell string or exec list for `command:`/`entrypoint:`.
//! [`StringOrList`] is a single string or list of strings (used in `dns:`, `cap_add:`, etc.).
//! [`Labels`] is the list or map form for `labels:`.
//! [`LoggingConfig`] is the `logging:` driver and options.
//! [`Sysctls`] is the list or map form for `sysctls:`.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Deserialize `extra_hosts` accepting either the list form (`["host:ip"]`) or
/// the mapping form (`{host: ip}`), normalizing both to `host:ip` strings so
/// the rest of the pipeline sees a single shape. Docker Compose accepts both.
pub(crate) fn deserialize_extra_hosts<'de, D>(de: D) -> Result<Vec<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum ListOrMap {
		List(Vec<String>),
		Map(IndexMap<String, String>),
	}
	Ok(match ListOrMap::deserialize(de)? {
		ListOrMap::List(v) => v,
		ListOrMap::Map(m) => m
			.into_iter()
			.map(|(host, ip)| format!("{host}:{ip}"))
			.collect(),
	})
}

/// Deserialize a field that the compose spec allows as either a YAML number or a
/// quoted string, normalizing to `Option<String>`. `cpus: 0.5` (number) and
/// `cpus: "0.5"` (string) both parse; an absent key stays `None`. Used for the
/// `cpus:` limits, which the spec writes unquoted but podup stores as a string.
pub(crate) fn deserialize_opt_string_or_number<'de, D>(de: D) -> Result<Option<String>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	#[derive(Deserialize)]
	#[serde(untagged)]
	enum StringOrNumber {
		Str(String),
		Int(i64),
		Float(f64),
	}
	Ok(Option::<StringOrNumber>::deserialize(de)?.map(|v| match v {
		StringOrNumber::Str(s) => s,
		StringOrNumber::Int(i) => i.to_string(),
		StringOrNumber::Float(fl) => fl.to_string(),
	}))
}

/// Container entrypoint / command: either a shell string or exec list.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Command {
	/// Shell form: a single string run via `sh -c`.
	Shell(String),
	/// Exec form: an explicit argument vector run without a shell.
	Exec(Vec<String>),
}

impl Command {
	/// Returns the command as an exec argument vector, wrapping a shell string in `sh -c`.
	pub fn to_exec(&self) -> Vec<String> {
		match self {
			Command::Shell(s) => vec!["sh".into(), "-c".into(), s.clone()],
			Command::Exec(v) => v.clone(),
		}
	}

	/// Returns the raw arguments without wrapping a shell string in `sh -c`.
	pub fn to_argv(&self) -> Vec<String> {
		match self {
			Command::Shell(s) => vec![s.clone()],
			Command::Exec(v) => v.clone(),
		}
	}
}

/// A field that accepts either a single string or a list of strings.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum StringOrList {
	/// The field was absent.
	#[default]
	Empty,
	/// A single string value.
	Single(String),
	/// A list of string values.
	List(Vec<String>),
}

impl StringOrList {
	/// Returns the value as a list of strings.
	pub fn to_list(&self) -> Vec<String> {
		match self {
			StringOrList::Empty => vec![],
			StringOrList::Single(s) => vec![s.clone()],
			StringOrList::List(v) => v.clone(),
		}
	}

	/// Returns whether the field holds no values.
	pub fn is_empty(&self) -> bool {
		match self {
			StringOrList::Empty => true,
			StringOrList::Single(s) => s.is_empty(),
			StringOrList::List(v) => v.is_empty(),
		}
	}
}

/// Labels: list or map form.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum Labels {
	/// The field was absent.
	#[default]
	Empty,
	/// List form: `KEY=VALUE` entries.
	List(Vec<String>),
	/// Map form: key-value pairs.
	Map(IndexMap<String, String>),
}

impl Labels {
	/// Returns the labels as a key-value map, splitting list entries on the first `=`.
	pub fn to_map(&self) -> HashMap<String, String> {
		match self {
			Labels::Empty => HashMap::new(),
			Labels::List(list) => list
				.iter()
				.filter_map(|s| {
					let mut parts = s.splitn(2, '=');
					Some((
						parts.next()?.to_string(),
						parts.next().unwrap_or("").to_string(),
					))
				})
				.collect(),
			Labels::Map(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
		}
	}

	/// Returns whether no labels are defined.
	pub fn is_empty(&self) -> bool {
		match self {
			Labels::Empty => true,
			Labels::List(v) => v.is_empty(),
			Labels::Map(m) => m.is_empty(),
		}
	}
}

/// `logging:` configuration: driver name and driver-specific options.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LoggingConfig {
	/// Logging driver name; the runtime default is used if absent.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,
	/// Driver-specific options.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub options: HashMap<String, String>,
}

/// Kernel parameters: list (`["net.ipv4.ip_forward=1"]`) or map form.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum Sysctls {
	/// The field was absent.
	#[default]
	Empty,
	/// List form: `key=value` entries.
	List(Vec<String>),
	/// Map form: kernel parameter keys to values.
	Map(IndexMap<String, serde_yaml::Value>),
}

impl Sysctls {
	/// Returns the sysctls as a key-value map, stringifying scalar values.
	pub fn to_map(&self) -> HashMap<String, String> {
		match self {
			Sysctls::Empty => HashMap::new(),
			Sysctls::List(list) => list
				.iter()
				.filter_map(|s| {
					let mut parts = s.splitn(2, '=');
					let key = parts.next()?.to_string();
					let val = parts.next().unwrap_or("").to_string();
					Some((key, val))
				})
				.collect(),
			Sysctls::Map(m) => m
				.iter()
				.map(|(k, v)| {
					let s = match v {
						serde_yaml::Value::String(s) => s.clone(),
						serde_yaml::Value::Number(n) => n.to_string(),
						serde_yaml::Value::Bool(b) => b.to_string(),
						_ => String::new(),
					};
					(k.clone(), s)
				})
				.collect(),
		}
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "primitives_tests.rs"]
mod tests;
