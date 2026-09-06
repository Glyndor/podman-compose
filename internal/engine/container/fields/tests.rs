//! Tests for `internal/engine/container/fields.rs`.
//!
//! Split out of `fields.rs` to keep that file within the source line limit.

use super::*;
use crate::compose::types::Service;

fn default_service() -> Service {
	Service::default()
}

// --- device parsing ---

#[test]
fn parse_device_host_container_perm() {
	let parsed = parse_device("/dev/null:/dev/zero:rwm");
	assert_eq!(parsed.device.path, "/dev/zero");
	// The trailing permission segment becomes a cgroup access rule.
	let rule = parsed.cgroup_rule.expect("perm should yield a cgroup rule");
	assert!(rule.allow);
	assert_eq!(rule.access.as_deref(), Some("rwm"));
}

#[test]
fn parse_device_same_path_both_sides() {
	let parsed = parse_device("/dev/null");
	assert_eq!(parsed.device.path, "/dev/null");
	// No permission segment → no cgroup rule (Podman defaults to rwm).
	assert!(parsed.cgroup_rule.is_none());
}

#[test]
fn parse_device_two_part() {
	let parsed = parse_device("/dev/null:/dev/xvda");
	assert_eq!(parsed.device.path, "/dev/xvda");
	assert!(parsed.cgroup_rule.is_none());
}

#[test]
fn parse_device_restricted_perm_is_preserved() {
	// `devices: ["/dev/sda:/dev/sda:r"]` must keep the read-only restriction
	// rather than silently becoming rwm on the live up path.
	let parsed = parse_device("/dev/sda:/dev/sda:r");
	assert_eq!(parsed.device.path, "/dev/sda");
	let rule = parsed.cgroup_rule.expect("perm should yield a cgroup rule");
	assert!(rule.allow);
	assert_eq!(rule.access.as_deref(), Some("r"));
	// The rule targets the same node as the device it restricts.
	assert_eq!(rule.device_type, Some(parsed.device.device_type));
	assert_eq!(rule.major, Some(parsed.device.major));
	assert_eq!(rule.minor, Some(parsed.device.minor));
}

// --- blkio ---

#[test]
fn build_blkio_config_empty_no_blkio() {
	assert!(build_blkio_config(&default_service()).is_none());
}

#[test]
fn build_blkio_config_weight_only() {
	use crate::compose::types::BlkioConfig;
	let mut svc = default_service();
	svc.blkio_config = Some(BlkioConfig {
		weight: Some(500),
		..Default::default()
	});
	let blkio = build_blkio_config(&svc).unwrap();
	assert_eq!(blkio.weight, Some(500));
	assert!(blkio.weight_device.is_empty());
}

#[test]
fn build_blkio_config_with_rate_device() {
	use crate::compose::types::{BlkioConfig, BlkioRateDevice};
	let mut svc = default_service();
	svc.blkio_config = Some(BlkioConfig {
		device_read_bps: vec![BlkioRateDevice {
			path: "/dev/sda".into(),
			rate: serde_yaml::Value::Number(serde_yaml::Number::from(1048576u64)),
		}],
		..Default::default()
	});
	let blkio = build_blkio_config(&svc).unwrap();
	assert_eq!(blkio.throttle_read_bps_device.len(), 1);
	assert_eq!(blkio.throttle_read_bps_device[0].rate, 1048576);
	assert!(blkio.throttle_write_bps_device.is_empty());
}

#[test]
fn build_blkio_config_maps_weight_device() {
	use crate::compose::types::{BlkioConfig, BlkioWeightDevice};
	let mut svc = default_service();
	svc.blkio_config = Some(BlkioConfig {
		weight: Some(300),
		weight_device: vec![BlkioWeightDevice {
			// A non-existent path stats to (0, 0); the weight still propagates.
			path: "/dev/does-not-exist".into(),
			weight: 800,
		}],
		..Default::default()
	});
	let blkio = build_blkio_config(&svc).unwrap();
	assert_eq!(blkio.weight, Some(300));
	assert_eq!(blkio.weight_device.len(), 1);
	assert_eq!(blkio.weight_device[0].weight, Some(800));
}

#[test]
fn build_blkio_config_maps_all_four_throttle_kinds() {
	use crate::compose::types::{BlkioConfig, BlkioRateDevice};
	let dev = |rate: u64| BlkioRateDevice {
		path: "/dev/sda".into(),
		rate: serde_yaml::Value::Number(serde_yaml::Number::from(rate)),
	};
	let mut svc = default_service();
	svc.blkio_config = Some(BlkioConfig {
		device_read_bps: vec![dev(1)],
		device_write_bps: vec![dev(2)],
		device_read_iops: vec![dev(3)],
		device_write_iops: vec![dev(4)],
		..Default::default()
	});
	let blkio = build_blkio_config(&svc).unwrap();
	assert_eq!(blkio.throttle_read_bps_device[0].rate, 1);
	assert_eq!(blkio.throttle_write_bps_device[0].rate, 2);
	assert_eq!(blkio.throttle_read_iops_device[0].rate, 3);
	assert_eq!(blkio.throttle_write_iops_device[0].rate, 4);
}

// --- build_label_file_labels ---

#[test]
fn label_file_parses_keys_skips_comments_and_blanks() {
	use crate::compose::types::primitives::StringOrList;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("labels.env");
	std::fs::write(
		&path,
		"# a comment\n\ncom.example.team=blue\nbare-key\n  com.example.tier = gold \n",
	)
	.unwrap();

	let mut svc = default_service();
	svc.label_file = StringOrList::Single("labels.env".to_string());
	let labels = build_label_file_labels(&svc, dir.path()).unwrap();

	assert_eq!(
		labels.get("com.example.team").map(String::as_str),
		Some("blue")
	);
	// A bare key with no `=` keeps an empty value.
	assert_eq!(labels.get("bare-key").map(String::as_str), Some(""));
	// The whole line is trimmed first, then the key side is trimmed again; the
	// value keeps its leading space after `=` but loses the line's trailing space.
	assert_eq!(
		labels.get("com.example.tier").map(String::as_str),
		Some(" gold")
	);
	// Comment and blank lines contribute nothing.
	assert_eq!(labels.len(), 3);
}

#[test]
fn label_file_missing_file_is_skipped() {
	use crate::compose::types::primitives::StringOrList;
	let dir = tempfile::tempdir().unwrap();
	let mut svc = default_service();
	svc.label_file = StringOrList::Single("absent.env".to_string());
	// A missing label file warns and yields no labels rather than erroring.
	assert!(build_label_file_labels(&svc, dir.path())
		.unwrap()
		.is_empty());
}

// --- warn_swarm_only_deploy ---

#[test]
fn warn_swarm_only_deploy_no_deploy_is_noop() {
	let svc = default_service();
	warn_swarm_only_deploy("web", &svc);
}

#[test]
fn warn_swarm_only_deploy_no_swarm_fields_is_noop() {
	use crate::compose::types::DeployConfig;
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		replicas: Some(2),
		..Default::default()
	});
	warn_swarm_only_deploy("web", &svc);
}

#[test]
fn warn_swarm_only_deploy_all_swarm_fields_no_panic() {
	use crate::compose::types::{DeployConfig, DeployPlacement, DeployUpdateConfig};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		mode: Some("global".to_string()),
		placement: Some(DeployPlacement {
			constraints: vec!["node.role == manager".to_string()],
			..Default::default()
		}),
		update_config: Some(DeployUpdateConfig {
			parallelism: Some(1),
			..Default::default()
		}),
		rollback_config: Some(DeployUpdateConfig::default()),
		endpoint_mode: Some("dnsrr".to_string()),
		..Default::default()
	});
	warn_swarm_only_deploy("web", &svc);
}

// --- container label resolution ---

#[test]
fn resolve_container_labels_keeps_service_labels() {
	use crate::compose::types::primitives::Labels;
	use indexmap::IndexMap;
	let mut svc = default_service();
	let mut map = IndexMap::new();
	map.insert("com.example.team".to_string(), "blue".to_string());
	svc.labels = Labels::Map(map);

	let labels = resolve_container_labels(&svc, HashMap::new());
	assert_eq!(
		labels.get("com.example.team").map(String::as_str),
		Some("blue")
	);
}

#[test]
fn resolve_container_labels_does_not_apply_deploy_labels() {
	use crate::compose::types::primitives::Labels;
	use crate::compose::types::DeployConfig;
	use indexmap::IndexMap;
	let mut svc = default_service();
	let mut svc_map = IndexMap::new();
	svc_map.insert("com.example.service".to_string(), "on".to_string());
	svc.labels = Labels::Map(svc_map);
	let mut deploy_map = IndexMap::new();
	deploy_map.insert("com.example.deploy".to_string(), "swarm".to_string());
	svc.deploy = Some(DeployConfig {
		labels: Labels::Map(deploy_map),
		..Default::default()
	});

	let labels = resolve_container_labels(&svc, HashMap::new());
	// Per the Compose Specification, deploy.labels are NOT applied to the container.
	assert!(!labels.contains_key("com.example.deploy"));
	// Service labels still apply.
	assert_eq!(
		labels.get("com.example.service").map(String::as_str),
		Some("on")
	);
}

#[test]
fn resolve_container_labels_service_overrides_label_file() {
	use crate::compose::types::primitives::Labels;
	use indexmap::IndexMap;
	let mut svc = default_service();
	let mut map = IndexMap::new();
	map.insert("shared".to_string(), "from-service".to_string());
	svc.labels = Labels::Map(map);
	let mut file_labels = HashMap::new();
	file_labels.insert("shared".to_string(), "from-file".to_string());
	file_labels.insert("only-file".to_string(), "yes".to_string());

	let labels = resolve_container_labels(&svc, file_labels);
	assert_eq!(
		labels.get("shared").map(String::as_str),
		Some("from-service")
	);
	assert_eq!(labels.get("only-file").map(String::as_str), Some("yes"));
}

// --- sanitize_kv_pair ---

#[test]
fn sanitize_kv_pair_accepts_normal_input() {
	let mut labels = HashMap::new();
	let (k, v) = sanitize_kv_pair(&mut labels, "com.example.team", "blue").unwrap();
	assert_eq!(k, "com.example.team");
	assert_eq!(v, "blue");
	assert_eq!(
		labels.get("com.example.team").map(String::as_str),
		Some("blue")
	);
}

#[test]
fn sanitize_kv_pair_rejects_empty_key() {
	let mut labels = HashMap::new();
	// An empty key would collide on every subsequent empty entry; reject at
	// the boundary rather than silently deduplicating.
	assert_eq!(
		sanitize_kv_pair(&mut labels, "", "v"),
		Err(SanitizeError::InvalidKey)
	);
	assert!(labels.is_empty());
}

#[test]
fn sanitize_kv_pair_rejects_control_char_in_key() {
	let mut labels = HashMap::new();
	// \t in the middle of the key: a downstream consumer that splits on
	// whitespace would see two keys.
	assert_eq!(
		sanitize_kv_pair(&mut labels, "bad\tkey", "v"),
		Err(SanitizeError::InvalidKey)
	);
	assert!(labels.is_empty());
}

#[test]
fn sanitize_kv_pair_rejects_control_char_in_value() {
	let mut labels = HashMap::new();
	// NUL in the value: meaningless in a label and would corrupt any
	// downstream parser that treats the value as a C string.
	assert_eq!(
		sanitize_kv_pair(&mut labels, "k", "bad\0val"),
		Err(SanitizeError::InvalidValue)
	);
	assert!(labels.is_empty());
}

#[test]
fn sanitize_kv_pair_rejects_oversize_key() {
	let mut labels = HashMap::new();
	// 254 bytes, one past the podman cap.
	let big = "a".repeat(MAX_LABEL_KEY_LEN + 1);
	assert_eq!(
		sanitize_kv_pair(&mut labels, &big, "v"),
		Err(SanitizeError::InvalidKey)
	);
	assert!(labels.is_empty());
}

#[test]
fn sanitize_kv_pair_accepts_key_at_cap() {
	let mut labels = HashMap::new();
	// Exactly the cap is the inclusive boundary.
	let at_cap = "a".repeat(MAX_LABEL_KEY_LEN);
	let (k, _) = sanitize_kv_pair(&mut labels, &at_cap, "v").unwrap();
	assert_eq!(k.len(), MAX_LABEL_KEY_LEN);
}

#[test]
fn sanitize_kv_pair_rejects_oversize_value() {
	let mut labels = HashMap::new();
	// 4 KiB + 1, past the value cap.
	let big = "a".repeat(MAX_LABEL_VALUE_LEN + 1);
	assert_eq!(
		sanitize_kv_pair(&mut labels, "k", &big),
		Err(SanitizeError::InvalidValue)
	);
	assert!(labels.is_empty());
}

#[test]
fn sanitize_kv_pair_rejects_when_map_is_full() {
	let mut labels = HashMap::new();
	for i in 0..MAX_LABEL_FILE_ENTRIES {
		labels.insert(format!("k{i}"), "v".into());
	}
	// A new key at the cap is rejected.
	assert_eq!(
		sanitize_kv_pair(&mut labels, "new-key", "v"),
		Err(SanitizeError::TooManyEntries)
	);
	// ...but overwriting an existing key at the cap is allowed: the cap
	// bounds the number of distinct keys, not total insertions.
	assert!(sanitize_kv_pair(&mut labels, "k0", "new-v").is_ok());
	assert_eq!(labels.get("k0").map(String::as_str), Some("new-v"));
}

// --- build_label_file_labels: integration ---

#[test]
fn label_file_rejects_control_char_in_value_at_parse() {
	use crate::compose::types::primitives::StringOrList;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("labels.env");
	// The value carries an embedded NUL: meaningless as a label and would
	// corrupt any downstream consumer that re-parses the value.
	std::fs::write(&path, "com.example.team=bl\0ue\n").unwrap();

	let mut svc = default_service();
	svc.label_file = StringOrList::Single("labels.env".to_string());
	let err = build_label_file_labels(&svc, dir.path()).unwrap_err();
	// The error names the file and the line so the user can find the
	// offending entry without bisecting the file.
	let msg = err.to_string();
	assert!(
		msg.contains("labels.env") && msg.contains("line 1"),
		"error did not name the file and line: {msg}",
	);
}

#[test]
fn label_file_caps_at_max_entries() {
	use crate::compose::types::primitives::StringOrList;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("labels.env");
	// A pathological file well past the entry cap. The 16 MiB read cap does
	// not constrain the resulting HashMap size; the per-file entry cap
	// does, and 1000 lines is past it.
	let mut content = String::new();
	for i in 0..1000 {
		content.push_str(&format!("com.example.k{i}=v\n"));
	}
	std::fs::write(&path, content).unwrap();

	let mut svc = default_service();
	svc.label_file = StringOrList::Single("labels.env".to_string());
	// The cap is reached before the 1000th line, so the call returns Err
	// rather than a silently-truncated map.
	let err = build_label_file_labels(&svc, dir.path()).unwrap_err();
	assert!(
		err.to_string().contains("TooManyEntries"),
		"expected TooManyEntries, got: {err}",
	);
}

#[test]
fn label_file_accepts_exactly_max_entries() {
	use crate::compose::types::primitives::StringOrList;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("labels.env");
	// Exactly the cap is the inclusive boundary; every entry fits.
	let mut content = String::new();
	for i in 0..MAX_LABEL_FILE_ENTRIES {
		content.push_str(&format!("com.example.k{i}=v\n"));
	}
	std::fs::write(&path, content).unwrap();

	let mut svc = default_service();
	svc.label_file = StringOrList::Single("labels.env".to_string());
	let labels = build_label_file_labels(&svc, dir.path()).unwrap();
	assert_eq!(labels.len(), MAX_LABEL_FILE_ENTRIES);
}

#[test]
fn label_file_overwrite_does_not_count_against_cap() {
	use crate::compose::types::primitives::StringOrList;
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("labels.env");
	// First `MAX_LABEL_FILE_ENTRIES` distinct keys, then repeated overwrites
	// of the first key; the overwrites are allowed at the cap.
	let mut content = String::new();
	for i in 0..MAX_LABEL_FILE_ENTRIES {
		content.push_str(&format!("com.example.k{i}=v{i}\n"));
	}
	content.push_str("com.example.k0=overwrite\n");
	std::fs::write(&path, content).unwrap();

	let mut svc = default_service();
	svc.label_file = StringOrList::Single("labels.env".to_string());
	let labels = build_label_file_labels(&svc, dir.path()).unwrap();
	assert_eq!(labels.len(), MAX_LABEL_FILE_ENTRIES);
	assert_eq!(
		labels.get("com.example.k0").map(String::as_str),
		Some("overwrite")
	);
}

// --- encode_path_for_label ---

#[test]
fn encode_path_for_label_passes_plain_path() {
	assert_eq!(
		encode_path_for_label("/home/user/compose.yaml"),
		"/home/user/compose.yaml"
	);
}

#[test]
fn encode_path_for_label_encodes_comma() {
	// A `,` in the path would visually merge with the next entry when the
	// joined `podup.config-files` label is split back on `,`.
	assert_eq!(encode_path_for_label("a,b/c.yaml"), "a%2Cb/c.yaml");
}

#[test]
fn encode_path_for_label_encodes_multiple_commas() {
	assert_eq!(encode_path_for_label("a,b,c.yaml"), "a%2Cb%2Cc.yaml");
}
