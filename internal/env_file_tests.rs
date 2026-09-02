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
	let file_env: HashMap<String, String> = [("FOO".to_string(), "from-file".to_string())].into();
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
	let service_env: HashMap<String, Option<String>> = [("PASSTHROUGH".to_string(), None)].into();
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
