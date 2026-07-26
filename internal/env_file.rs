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
/// at all — so the one command you reach for to ask "what will this actually run"
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
mod tests {
	use super::*;
	use crate::compose::types::EnvFileEntry;

	// load_env_file_entries

	#[test]
	fn loads_key_value_pairs() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join(".env"), "FOO=bar\nBAZ=qux\n").unwrap();
		let entries = vec![EnvFileEntry::Path(".env".into())];
		let m = load_env_file_entries(&entries, dir.path()).unwrap();
		assert_eq!(m.get("FOO").map(|s| s.as_str()), Some("bar"));
		assert_eq!(m.get("BAZ").map(|s| s.as_str()), Some("qux"));
	}

	#[test]
	fn skips_comments_and_blank_lines() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join(".env"), "# comment\n\nFOO=bar\n").unwrap();
		let entries = vec![EnvFileEntry::Path(".env".into())];
		let m = load_env_file_entries(&entries, dir.path()).unwrap();
		assert_eq!(m.len(), 1);
	}

	#[test]
	fn bare_key_passes_through_host_or_is_omitted() {
		// A bare key (no `=`) takes its value from the host environment; absent
		// from the host, it is omitted rather than set to an empty string.
		std::env::set_var("PODUP_ENVFILE_BARE_PRESENT", "h");
		std::env::remove_var("PODUP_ENVFILE_BARE_ABSENT");
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(
			dir.path().join(".env"),
			"PODUP_ENVFILE_BARE_PRESENT\nPODUP_ENVFILE_BARE_ABSENT\n",
		)
		.unwrap();
		let entries = vec![EnvFileEntry::Path(".env".into())];
		let m = load_env_file_entries(&entries, dir.path()).unwrap();
		assert_eq!(
			m.get("PODUP_ENVFILE_BARE_PRESENT").map(|s| s.as_str()),
			Some("h")
		);
		assert!(!m.contains_key("PODUP_ENVFILE_BARE_ABSENT"));
		std::env::remove_var("PODUP_ENVFILE_BARE_PRESENT");
	}

	#[test]
	fn last_file_wins_on_duplicate_key() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("a.env"), "FOO=first\n").unwrap();
		std::fs::write(dir.path().join("b.env"), "FOO=second\n").unwrap();
		let entries = vec![
			EnvFileEntry::Path("a.env".into()),
			EnvFileEntry::Path("b.env".into()),
		];
		let m = load_env_file_entries(&entries, dir.path()).unwrap();
		assert_eq!(m.get("FOO").map(|s| s.as_str()), Some("second"));
	}

	#[test]
	fn missing_required_file_returns_error() {
		let dir = tempfile::tempdir().unwrap();
		let entries = vec![EnvFileEntry::Path("nonexistent.env".into())];
		assert!(load_env_file_entries(&entries, dir.path()).is_err());
	}

	#[test]
	fn missing_optional_file_skipped() {
		let dir = tempfile::tempdir().unwrap();
		let entries = vec![EnvFileEntry::Config {
			path: "nonexistent.env".into(),
			required: Some(false),
			format: None,
		}];
		let m = load_env_file_entries(&entries, dir.path()).unwrap();
		assert!(m.is_empty());
	}

	#[test]
	fn non_dotenv_format_warns_and_parses_as_dotenv() {
		// compose-go logs a warning for an unknown `format` and falls back to
		// dotenv parsing rather than failing; podup must not error here.
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join(".env"), "FOO=bar\n").unwrap();
		let entries = vec![EnvFileEntry::Config {
			path: ".env".into(),
			required: Some(false),
			format: Some("json".into()),
		}];
		let m = load_env_file_entries(&entries, dir.path()).unwrap();
		assert_eq!(m.get("FOO").map(|s| s.as_str()), Some("bar"));
	}

	#[test]
	fn unterminated_quote_is_an_error() {
		// A never-closed quote would otherwise absorb every following key; an
		// explicitly requested env_file must fail loudly instead.
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join(".env"), "A=\"oops\nB=keep\n").unwrap();
		let entries = vec![EnvFileEntry::Path(".env".into())];
		let err = load_env_file_entries(&entries, dir.path()).unwrap_err();
		assert!(matches!(err, ComposeError::EnvFile(_)), "got {err:?}");
	}

	#[test]
	fn strips_leading_bom_first_key_kept() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join(".env"), "\u{feff}FOO=bar\n").unwrap();
		let entries = vec![EnvFileEntry::Path(".env".into())];
		let m = load_env_file_entries(&entries, dir.path()).unwrap();
		assert_eq!(m.get("FOO").map(|s| s.as_str()), Some("bar"));
	}

	#[test]
	fn loads_parent_relative_env_file() {
		// docker-compose, podman, and podman-compose all accept env_file paths
		// outside the project directory (e.g. a shared `../secrets/.env` in a
		// monorepo); podup must too.
		let root = tempfile::tempdir().unwrap();
		std::fs::write(root.path().join("shared.env"), "FOO=bar\n").unwrap();
		let project = root.path().join("project");
		std::fs::create_dir(&project).unwrap();
		let entries = vec![EnvFileEntry::Path("../shared.env".into())];
		let m = load_env_file_entries(&entries, &project).unwrap();
		assert_eq!(m.get("FOO").map(|s| s.as_str()), Some("bar"));
	}

	// merge_env

	#[test]
	fn service_env_wins_over_file_env() {
		let service_env: HashMap<String, Option<String>> =
			[("FOO".to_string(), Some("from-service".to_string()))].into();
		let file_env: HashMap<String, String> =
			[("FOO".to_string(), "from-file".to_string())].into();
		let result = merge_env(service_env, file_env);
		let foo_entry = result
			.iter()
			.find(|s| s.starts_with("FOO="))
			.unwrap()
			.clone();
		assert_eq!(foo_entry, "FOO=from-service");
	}

	#[test]
	fn file_env_fills_missing_keys() {
		let service_env: HashMap<String, Option<String>> = HashMap::new();
		let file_env: HashMap<String, String> = [("BAR".to_string(), "baz".to_string())].into();
		let result = merge_env(service_env, file_env);
		assert!(result.iter().any(|s| s == "BAR=baz"));
	}

	#[test]
	fn key_only_env_var_has_no_equals() {
		let service_env: HashMap<String, Option<String>> =
			[("PASSTHROUGH".to_string(), None)].into();
		let result = merge_env(service_env, HashMap::new());
		assert!(result.iter().any(|s| s == "PASSTHROUGH"));
	}

	// materialize_env_files (#1184)

	/// Parse `yaml`, materialise its env files against `dir`, and return the
	/// service's rendered `environment` map.
	fn materialised(
		dir: &std::path::Path,
		yaml: &str,
	) -> indexmap::IndexMap<String, Option<serde_yaml::Value>> {
		let mut file = crate::compose::parse_str_raw(yaml).unwrap();
		materialize_env_files(&mut file, dir).unwrap();
		let service = &file.services["web"];
		assert!(
			matches!(service.env_file, crate::compose::types::EnvFile::Empty),
			"env_file must be dropped once it has been folded in"
		);
		match &service.environment {
			crate::compose::types::EnvVars::Map(m) => m.clone(),
			other => panic!("expected a map, got {other:?}"),
		}
	}

	#[test]
	fn env_file_is_folded_into_environment() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("a.env"), b"FROM_FILE=yes\n").unwrap();
		let vars = materialised(
			dir.path(),
			"services:\n  web:\n    image: x\n    env_file:\n      - a.env\n",
		);
		assert_eq!(
			vars.get("FROM_FILE"),
			Some(&Some(serde_yaml::Value::String("yes".into())))
		);
	}

	#[test]
	fn environment_wins_over_env_file() {
		// The run-time precedence, kept in the rendered model: a key set in both
		// places must render with the value the container would actually see.
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("a.env"), b"SHARED=from-file\n").unwrap();
		let vars = materialised(
			dir.path(),
			"services:\n  web:\n    image: x\n    environment:\n      SHARED: from-service\n    env_file:\n      - a.env\n",
		);
		assert_eq!(
			vars.get("SHARED"),
			Some(&Some(serde_yaml::Value::String("from-service".into())))
		);
	}

	#[test]
	fn a_later_env_file_wins_over_an_earlier_one() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("a.env"), b"K=first\n").unwrap();
		std::fs::write(dir.path().join("b.env"), b"K=second\n").unwrap();
		let vars = materialised(
			dir.path(),
			"services:\n  web:\n    image: x\n    env_file:\n      - a.env\n      - b.env\n",
		);
		assert_eq!(
			vars.get("K"),
			Some(&Some(serde_yaml::Value::String("second".into())))
		);
	}

	#[test]
	fn a_bare_key_stays_valueless() {
		// `KEY` with no value inherits from the host. Rendering it as an empty
		// string instead would change what the model means.
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("a.env"), b"OTHER=1\n").unwrap();
		let vars = materialised(
			dir.path(),
			"services:\n  web:\n    image: x\n    environment:\n      - PASSTHROUGH\n    env_file:\n      - a.env\n",
		);
		assert_eq!(vars.get("PASSTHROUGH"), Some(&None));
	}

	#[test]
	fn keys_are_sorted_so_the_render_is_stable() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("a.env"), b"ZED=1\nALPHA=2\nMID=3\n").unwrap();
		let vars = materialised(
			dir.path(),
			"services:\n  web:\n    image: x\n    env_file:\n      - a.env\n",
		);
		let keys: Vec<&String> = vars.keys().collect();
		assert_eq!(keys, vec!["ALPHA", "MID", "ZED"]);
	}

	#[test]
	fn a_service_without_env_file_is_left_alone() {
		let dir = tempfile::tempdir().unwrap();
		let mut file = crate::compose::parse_str_raw(
			"services:\n  web:\n    image: x\n    environment:\n      A: 1\n",
		)
		.unwrap();
		let before = format!("{:?}", file.services["web"].environment);
		materialize_env_files(&mut file, dir.path()).unwrap();
		assert_eq!(
			before,
			format!("{:?}", file.services["web"].environment),
			"a service with no env_file must not be rewritten"
		);
	}
}
