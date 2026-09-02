use super::*;
use std::collections::HashMap;

/// The modes every slot shares.
#[test]
fn namespace_modes_accept_allow_list() {
	for field in [PID_FIELD, IPC_FIELD, UTS_FIELD, USERNS_FIELD, CGROUP_FIELD] {
		assert!(is_valid_namespace_mode(field, "host"), "{field}");
		assert!(is_valid_namespace_mode(field, "private"), "{field}");
		assert!(is_valid_namespace_mode(field, "pod"), "{field}");
		assert!(is_valid_namespace_mode(field, "container:abc"), "{field}");
		assert!(
			is_valid_namespace_mode(field, "ns:/run/netns/foo"),
			"{field}"
		);
	}
}

#[test]
fn namespace_modes_reject_unknown() {
	assert!(!is_valid_namespace_mode(PID_FIELD, "evil"));
	assert!(!is_valid_namespace_mode(PID_FIELD, ""));
	assert!(!is_valid_namespace_mode(PID_FIELD, "HOST"));
	assert!(!is_valid_namespace_mode(PID_FIELD, "container:"));
	assert!(!is_valid_namespace_mode(PID_FIELD, "ns:"));
}

/// A one-character path is still a path. The old check measured every
/// prefix against `"container:".len()`, so `ns:/x` failed for being short
/// rather than for being wrong.
#[test]
fn a_short_ns_path_is_accepted() {
	assert!(is_valid_namespace_mode(PID_FIELD, "ns:/x"));
}

/// Measured against podman 5.7.0: `shareable` and `none` parse for `ipc`
/// and are rejected by every other slot with "unrecognized namespace
/// mode". `shareable` is the one compose files reach for, and podup used
/// to refuse it.
#[test]
fn ipc_takes_shareable_and_none_but_no_other_slot_does() {
	assert!(is_valid_namespace_mode(IPC_FIELD, "shareable"));
	assert!(is_valid_namespace_mode(IPC_FIELD, "none"));
	for field in [PID_FIELD, UTS_FIELD, USERNS_FIELD, CGROUP_FIELD] {
		assert!(!is_valid_namespace_mode(field, "shareable"), "{field}");
		assert!(!is_valid_namespace_mode(field, "none"), "{field}");
	}
}

/// Measured against podman 5.7.0. `keep-id` is the standard rootless
/// answer to a file-ownership mismatch, and podup rejected it outright
/// (#1463). The option-carrying forms parse too.
#[test]
fn userns_takes_its_own_vocabulary() {
	for mode in [
		"keep-id",
		"auto",
		"nomap",
		"keep-id:uid=1000,gid=1000",
		"auto:size=65536",
	] {
		assert!(is_valid_namespace_mode(USERNS_FIELD, mode), "{mode}");
	}
	// And nowhere else.
	for field in [PID_FIELD, IPC_FIELD, UTS_FIELD, CGROUP_FIELD] {
		assert!(!is_valid_namespace_mode(field, "keep-id"), "{field}");
		assert!(!is_valid_namespace_mode(field, "auto"), "{field}");
		assert!(!is_valid_namespace_mode(field, "nomap"), "{field}");
	}
	// The option form still needs an option after the colon.
	assert!(!is_valid_namespace_mode(USERNS_FIELD, "keep-id:"));
	assert!(!is_valid_namespace_mode(USERNS_FIELD, "auto:"));
}

/// The error text has to name the modes the slot in hand accepts, not a
/// union that would send the reader after a value their slot rejects.
#[test]
fn the_error_lists_the_modes_for_that_slot() {
	let userns = allowed_namespace_modes(USERNS_FIELD);
	assert!(userns.contains("keep-id"), "{userns}");
	assert!(!userns.contains("shareable"), "{userns}");

	let ipc = allowed_namespace_modes(IPC_FIELD);
	assert!(ipc.contains("shareable"), "{ipc}");
	assert!(!ipc.contains("keep-id"), "{ipc}");

	let pid = allowed_namespace_modes(PID_FIELD);
	assert!(!pid.contains("keep-id"), "{pid}");
	assert!(!pid.contains("shareable"), "{pid}");
}

#[test]
fn first_invalid_namespace_returns_first_failure() {
	let slots: Vec<(&str, Option<&str>)> = vec![
		(PID_FIELD, Some("host")),
		(IPC_FIELD, Some("evil")),
		(UTS_FIELD, Some("private")),
	];
	let (field, value, allowed) = first_invalid_namespace(&slots).unwrap();
	assert_eq!(field, IPC_FIELD);
	assert_eq!(value, "evil");
	assert!(allowed.contains("`host`"));
	assert!(allowed.contains("`private`"));
	assert!(allowed.contains("`container:<id-or-path>`"));
}

#[test]
fn first_invalid_namespace_passes_when_all_valid() {
	let slots: Vec<(&str, Option<&str>)> = vec![
		(PID_FIELD, Some("host")),
		(IPC_FIELD, Some("container:web")),
		(UTS_FIELD, None),
	];
	assert!(first_invalid_namespace(&slots).is_none());
}

#[test]
fn device_access_accepts_rwm_subsets() {
	assert!(is_valid_device_access("r"));
	assert!(is_valid_device_access("w"));
	assert!(is_valid_device_access("m"));
	assert!(is_valid_device_access("rw"));
	assert!(is_valid_device_access("rwm"));
	assert!(is_valid_device_access("wm"));
}

#[test]
fn device_access_rejects_invalid_chars() {
	assert!(!is_valid_device_access("x"));
	assert!(!is_valid_device_access("rwx"));
	assert!(!is_valid_device_access("r w"));
	assert!(!is_valid_device_access("rw "));
}

#[test]
fn first_invalid_device_access_returns_index_and_field() {
	let rules = ["rwm", "rwmx", "rw"];
	let (field, value) = first_invalid_device_access(rules.iter().copied()).unwrap();
	assert_eq!(field, "device_cgroup_rule[1].access");
	assert_eq!(value, "rwmx");
}

#[test]
fn first_invalid_device_access_skips_empty() {
	let rules = ["rwm", "", "rwx"];
	let (field, _) = first_invalid_device_access(rules.iter().copied()).unwrap();
	assert_eq!(field, "device_cgroup_rule[2].access");
}

#[test]
fn first_invalid_device_access_passes_when_all_valid() {
	let rules = ["rwm", "r", "wm"];
	assert!(first_invalid_device_access(rules.iter().copied()).is_none());
}

#[test]
fn kv_key_accepts_alnum_dot_dash_underscore() {
	assert!(is_valid_kv_key("FOO"));
	assert!(is_valid_kv_key("foo_bar"));
	assert!(is_valid_kv_key("FOO.BAR"));
	assert!(is_valid_kv_key("FOO-BAR"));
	assert!(is_valid_kv_key("a1b2c3"));
}

#[test]
fn kv_key_rejects_invalid() {
	assert!(!is_valid_kv_key(""));
	assert!(!is_valid_kv_key("FOO BAR"));
	assert!(!is_valid_kv_key("FOO\nBAR"));
	assert!(!is_valid_kv_key("FOO=BAR"));
	assert!(!is_valid_kv_key("FOO;ls"));
}

#[test]
fn first_invalid_kv_key_returns_first_failure() {
	let keys = ["GOOD", "MALFORMED\nKEY", "ALSO_GOOD"];
	let (field, key, msg) = first_invalid_kv_key("build.args", keys.iter().copied()).unwrap();
	assert_eq!(field, "build.args");
	assert_eq!(key, "MALFORMED\nKEY");
	assert!(msg.contains("not a valid identifier"));
}

#[test]
fn first_invalid_kv_key_passes_when_all_valid() {
	let keys = ["GOOD", "ALSO_GOOD"];
	assert!(first_invalid_kv_key("build.args", keys.iter().copied()).is_none());
}

#[test]
fn pre_validate_build_rejects_bad_key() {
	let mut args = HashMap::new();
	args.insert("MALFORMED\nKEY".to_string(), "value".to_string());
	let labels = HashMap::new();
	let err = pre_validate_build(&args, &labels).unwrap_err();
	let msg = err.to_string();
	assert!(msg.contains("build.args"));
	assert!(msg.contains("MALFORMED\\nKEY"));
}

#[test]
fn pre_validate_build_rejects_bad_label() {
	let args = HashMap::new();
	let mut labels = HashMap::new();
	labels.insert("bad key".to_string(), "value".to_string());
	let err = pre_validate_build(&args, &labels).unwrap_err();
	let msg = err.to_string();
	assert!(msg.contains("build.labels"));
	assert!(msg.contains("bad key"));
}

#[test]
fn pre_validate_build_passes_when_clean() {
	let mut args = HashMap::new();
	args.insert("GOOD".to_string(), "value".to_string());
	let mut labels = HashMap::new();
	labels.insert("GOOD_LABEL".to_string(), "value".to_string());
	assert!(pre_validate_build(&args, &labels).is_ok());
}

#[test]
fn render_value_truncates_long_inputs() {
	let long = "x".repeat(500);
	let r = render_value(&long);
	assert!(r.len() <= 260);
	assert!(r.ends_with('…'));
}

#[test]
fn render_value_escapes_control_chars() {
	assert_eq!(render_value("a\nb"), "a\\nb");
	assert_eq!(render_value("a\tb"), "a\\tb");
	assert_eq!(render_value("a\rb"), "a\\rb");
	assert!(render_value("a\x1bb").contains("\\u{1b}"));
}

#[test]
fn render_value_keeps_normal_strings_intact() {
	assert_eq!(render_value("hello"), "hello");
	assert_eq!(render_value("CAP_NET_ADMIN"), "CAP_NET_ADMIN");
	assert_eq!(render_value("db:10.0.0.2"), "db:10.0.0.2");
}

#[test]
fn build_field_error_uses_empty_service() {
	let e = build_field_error("build.args", "MALFORMED\nKEY", "podman rejected the key");
	match e {
		PodmanError::Field {
			service,
			field,
			value,
			message,
		} => {
			assert_eq!(service, "");
			assert_eq!(field, "build.args");
			assert_eq!(value, "MALFORMED\\nKEY");
			assert_eq!(message, "podman rejected the key");
		}
		_ => panic!("expected Field variant"),
	}
}

#[test]
fn spec_field_error_carries_service() {
	let e = spec_field_error("web", "pid", "evil", "namespace not recognised");
	match e {
		PodmanError::Field {
			service,
			field,
			value,
			message,
		} => {
			assert_eq!(service, "web");
			assert_eq!(field, "pid");
			assert_eq!(value, "evil");
			assert_eq!(message, "namespace not recognised");
		}
		_ => panic!("expected Field variant"),
	}
}
