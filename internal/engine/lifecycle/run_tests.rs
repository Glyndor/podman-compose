use super::{merge_run_environment, write_frame};
use std::collections::HashMap;

fn lookup<'a>(list: &'a [String], key: &str) -> Option<&'a str> {
	// Mirror downstream "later duplicate wins" semantics.
	list.iter().rev().find_map(|e| match e.split_once('=') {
		Some((k, v)) if k == key => Some(v),
		_ => None,
	})
}

#[test]
fn env_file_seeds_environment() {
	let file: HashMap<String, String> = [("FOO".to_string(), "from-file".to_string())].into();
	let list = merge_run_environment(file, HashMap::new(), Vec::new());
	assert_eq!(lookup(&list, "FOO"), Some("from-file"));
}

#[test]
fn service_environment_overrides_env_file() {
	let file: HashMap<String, String> = [("FOO".to_string(), "from-file".to_string())].into();
	let service: HashMap<String, Option<String>> =
		[("FOO".to_string(), Some("from-service".to_string()))].into();
	let list = merge_run_environment(file, service, Vec::new());
	assert_eq!(lookup(&list, "FOO"), Some("from-service"));
}

#[test]
fn dash_e_override_wins_over_all() {
	let file: HashMap<String, String> = [("FOO".to_string(), "from-file".to_string())].into();
	let service: HashMap<String, Option<String>> =
		[("FOO".to_string(), Some("from-service".to_string()))].into();
	let list = merge_run_environment(file, service, vec!["FOO=from-cli".to_string()]);
	assert_eq!(lookup(&list, "FOO"), Some("from-cli"));
}

#[test]
fn distinct_keys_from_each_layer_are_kept() {
	let file: HashMap<String, String> = [("A".to_string(), "a".to_string())].into();
	let service: HashMap<String, Option<String>> =
		[("B".to_string(), Some("b".to_string()))].into();
	let list = merge_run_environment(file, service, vec!["C=c".to_string()]);
	assert_eq!(lookup(&list, "A"), Some("a"));
	assert_eq!(lookup(&list, "B"), Some("b"));
	assert_eq!(lookup(&list, "C"), Some("c"));
}

// `write_frame` (#1364)

#[test]
fn write_frame_valid_utf8_writes_bytes_verbatim() {
	let mut buf = Vec::new();
	write_frame(&mut buf, b"hello\nworld\n").unwrap();
	assert_eq!(buf, b"hello\nworld\n");
}

#[test]
fn write_frame_invalid_utf8_substitutes_replacement() {
	let mut buf = Vec::new();
	write_frame(&mut buf, b"ok\xFF!\n").unwrap();
	assert_eq!(buf, "ok�!\n".as_bytes());
}
