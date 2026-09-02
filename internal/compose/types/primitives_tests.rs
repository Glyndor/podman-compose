use super::*;
use indexmap::IndexMap;

// Command

#[test]
fn command_shell_to_exec_wraps_in_sh() {
	let cmd = Command::Shell("echo hi".into());
	assert_eq!(cmd.to_exec(), vec!["sh", "-c", "echo hi"]);
}

#[test]
fn command_exec_to_exec_passthrough() {
	let cmd = Command::Exec(vec!["ls".into(), "-la".into()]);
	assert_eq!(cmd.to_exec(), vec!["ls", "-la"]);
}

#[test]
fn command_shell_to_argv_returns_shell_string() {
	let cmd = Command::Shell("echo hi".into());
	assert_eq!(cmd.to_argv(), vec!["echo hi"]);
}

#[test]
fn command_exec_to_argv_passthrough() {
	let cmd = Command::Exec(vec!["ls".into()]);
	assert_eq!(cmd.to_argv(), vec!["ls"]);
}

// string-or-number (cpus)

#[derive(Deserialize)]
struct CpusHolder {
	#[serde(default, deserialize_with = "deserialize_opt_string_or_number")]
	cpus: Option<String>,
}

#[test]
fn opt_string_or_number_accepts_unquoted_float() {
	let h: CpusHolder = serde_yaml::from_str("cpus: 0.5\n").unwrap();
	assert_eq!(h.cpus.as_deref(), Some("0.5"));
}

#[test]
fn opt_string_or_number_accepts_quoted_string() {
	let h: CpusHolder = serde_yaml::from_str("cpus: \"0.5\"\n").unwrap();
	assert_eq!(h.cpus.as_deref(), Some("0.5"));
}

#[test]
fn opt_string_or_number_accepts_integer() {
	let h: CpusHolder = serde_yaml::from_str("cpus: 2\n").unwrap();
	assert_eq!(h.cpus.as_deref(), Some("2"));
}

#[test]
fn opt_string_or_number_absent_is_none() {
	let h: CpusHolder = serde_yaml::from_str("other: 1\n").unwrap();
	assert_eq!(h.cpus, None);
}

// StringOrList

#[test]
fn string_or_list_empty_to_list() {
	assert!(StringOrList::Empty.to_list().is_empty());
}

#[test]
fn string_or_list_single_to_list() {
	assert_eq!(StringOrList::Single("a".into()).to_list(), vec!["a"]);
}

#[test]
fn string_or_list_list_to_list() {
	let s = StringOrList::List(vec!["a".into(), "b".into()]);
	assert_eq!(s.to_list(), vec!["a", "b"]);
}

#[test]
fn string_or_list_empty_is_empty() {
	assert!(StringOrList::Empty.is_empty());
}

#[test]
fn string_or_list_single_empty_string_is_empty() {
	assert!(StringOrList::Single(String::new()).is_empty());
}

#[test]
fn string_or_list_nonempty_single_not_empty() {
	assert!(!StringOrList::Single("x".into()).is_empty());
}

// Labels

#[test]
fn labels_empty_to_map() {
	assert!(Labels::Empty.to_map().is_empty());
}

#[test]
fn labels_list_parses_key_equals_value() {
	let l = Labels::List(vec!["env=prod".into(), "team=infra".into()]);
	let m = l.to_map();
	assert_eq!(m.get("env").map(|s| s.as_str()), Some("prod"));
	assert_eq!(m.get("team").map(|s| s.as_str()), Some("infra"));
}

#[test]
fn labels_list_key_only_has_empty_value() {
	let l = Labels::List(vec!["bare".into()]);
	let m = l.to_map();
	assert_eq!(m.get("bare").map(|s| s.as_str()), Some(""));
}

#[test]
fn labels_map_to_map() {
	let mut im = IndexMap::new();
	im.insert("k".to_string(), "v".to_string());
	let m = Labels::Map(im).to_map();
	assert_eq!(m.get("k").map(|s| s.as_str()), Some("v"));
}

#[test]
fn labels_is_empty_variants() {
	assert!(Labels::Empty.is_empty());
	assert!(Labels::List(vec![]).is_empty());
	let mut im = IndexMap::new();
	im.insert("x".to_string(), "y".to_string());
	assert!(!Labels::Map(im).is_empty());
}

// Sysctls

#[test]
fn sysctls_empty_to_map() {
	assert!(Sysctls::Empty.to_map().is_empty());
}

#[test]
fn sysctls_list_parses() {
	let s = Sysctls::List(vec!["net.ipv4.ip_forward=1".into()]);
	let m = s.to_map();
	assert_eq!(m.get("net.ipv4.ip_forward").map(|s| s.as_str()), Some("1"));
}

#[test]
fn sysctls_map_string_value() {
	let mut im = IndexMap::new();
	im.insert(
		"net.core.somaxconn".to_string(),
		serde_yaml::Value::Number(128.into()),
	);
	let m = Sysctls::Map(im).to_map();
	assert_eq!(m.get("net.core.somaxconn").map(|s| s.as_str()), Some("128"));
}
