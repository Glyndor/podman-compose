use std::collections::{BTreeSet, HashMap};

use super::*;
use crate::compose::types::{
	BindOptions, CountOrAll, DeviceReservation, DriverConfig, Labels, ResourceSpec,
	ServiceNetworkConfig, TmpfsOptions, VolumeOptions,
};

/// Interpolate + merge-resolve `yaml` exactly as the parser does, then diff
/// the nested blocks, mirroring the production caller, which feeds the pure
/// entry the interpolated document text.
fn warnings_for(yaml: &str) -> Vec<String> {
	let value = crate::compose::merge::interpolated_value(yaml, None).unwrap();
	let text = serde_yaml::to_string(&value).unwrap();
	raw_nested_unknown_warnings(&text)
}

/// Keys serde actually writes for a value, as a set.
fn serialized_keys<T: serde::Serialize>(value: &T) -> BTreeSet<String> {
	serde_yaml::to_value(value)
		.unwrap()
		.as_mapping()
		.unwrap()
		.keys()
		.map(|k| k.as_str().unwrap().to_string())
		.collect()
}

fn allowlist(keys: &[&str]) -> BTreeSet<String> {
	keys.iter().map(|s| s.to_string()).collect()
}

fn one_entry_map() -> HashMap<String, String> {
	HashMap::from([("k".to_string(), "v".to_string())])
}

// --- Drift guards: an exhaustive struct literal (no `..Default::default()`)
// forces a compile error if a field is added, until the allowlist is updated.

#[test]
fn bind_options_allowlist_matches_serde() {
	let v = BindOptions {
		propagation: Some("rprivate".to_string()),
		create_host_path: Some(true),
		selinux: Some("z".to_string()),
	};
	assert_eq!(serialized_keys(&v), allowlist(BIND_OPTIONS_KEYS));
}

#[test]
fn volume_options_allowlist_matches_serde() {
	let v = VolumeOptions {
		nocopy: Some(true),
		labels: Labels::List(vec!["a=b".to_string()]),
		driver_config: Some(DriverConfig {
			name: Some("local".to_string()),
			options: one_entry_map(),
		}),
		subpath: Some("sub".to_string()),
		noexec: Some(true),
		nosuid: Some(true),
		nodev: Some(true),
	};
	assert_eq!(serialized_keys(&v), allowlist(VOLUME_OPTIONS_KEYS));
}

#[test]
fn driver_config_allowlist_matches_serde() {
	let v = DriverConfig {
		name: Some("local".to_string()),
		options: one_entry_map(),
	};
	assert_eq!(serialized_keys(&v), allowlist(DRIVER_CONFIG_KEYS));
}

#[test]
fn tmpfs_options_allowlist_matches_serde() {
	let v = TmpfsOptions {
		size: Some(1024),
		mode: Some(0o755),
	};
	assert_eq!(serialized_keys(&v), allowlist(TMPFS_OPTIONS_KEYS));
}

#[test]
fn service_network_config_allowlist_matches_serde() {
	let v = ServiceNetworkConfig {
		aliases: Some(vec!["a".to_string()]),
		ipv4_address: Some("10.0.0.2".to_string()),
		ipv6_address: Some("::1".to_string()),
		link_local_ips: vec!["169.254.0.1".to_string()],
		priority: Some(1),
		mac_address: Some("02:42:ac:11:00:02".to_string()),
		driver_opts: one_entry_map(),
		gw_priority: Some(2),
		interface_name: Some("eth0".to_string()),
	};
	assert_eq!(serialized_keys(&v), allowlist(SERVICE_NETWORK_CONFIG_KEYS));
}

#[test]
fn resource_spec_allowlist_matches_serde() {
	let v = ResourceSpec {
		cpus: Some("0.5".to_string()),
		memory: Some("512M".to_string()),
		pids: Some(100),
		devices: vec![DeviceReservation {
			capabilities: vec!["gpu".to_string()],
			count: Some(CountOrAll::N(1)),
			device_ids: vec!["0".to_string()],
			driver: Some("nvidia".to_string()),
			options: one_entry_map(),
		}],
	};
	assert_eq!(serialized_keys(&v), allowlist(RESOURCE_SPEC_KEYS));
}

#[test]
fn device_reservation_allowlist_matches_serde() {
	let v = DeviceReservation {
		capabilities: vec!["gpu".to_string()],
		count: Some(CountOrAll::N(1)),
		device_ids: vec!["0".to_string()],
		driver: Some("nvidia".to_string()),
		options: one_entry_map(),
	};
	assert_eq!(serialized_keys(&v), allowlist(DEVICE_RESERVATION_KEYS));
}

// --- Positive: an unknown key in each block warns with the right context ---

#[test]
fn warns_on_unknown_bind_key() {
	// `create_hostpath` is a typo for `create_host_path` and is dropped silently
	// by `BindOptions`; it must be surfaced with the indexed context.
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: bind\n        source: /host\n        target: /in\n        bind:\n          create_hostpath: true\n",
	);
	assert!(
		msgs.iter().any(|m| m
			== "service 'web' volumes[0].bind: unknown key 'create_hostpath' is ignored (check for a typo or an unsupported compose feature)"),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_unknown_volume_key() {
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: volume\n        source: data\n        target: /data\n        volume:\n          nocpy: true\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("volumes[0].volume") && m.contains("nocpy")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_unknown_tmpfs_key() {
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: tmpfs\n        target: /t\n        tmpfs:\n          siz: 1024\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("volumes[0].tmpfs") && m.contains("siz")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_unknown_driver_config_key_via_recursion() {
	// The unknown key lives one level below `volume`, which the parent
	// allowlist cannot reach; only the recursion into `driver_config` finds it.
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: volume\n        source: data\n        target: /data\n        volume:\n          driver_config:\n            name: local\n            optoins: {}\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("volumes[0].volume.driver_config") && m.contains("optoins")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_unknown_deploy_limits_key() {
	let msgs = warnings_for(
		"services:\n  db:\n    image: pg\n    deploy:\n      resources:\n        limits:\n          cpus: '0.5'\n          memroy: 512M\n",
	);
	assert!(
		msgs.iter().any(|m| m
			== "service 'db' deploy.resources.limits: unknown key 'memroy' is ignored (check for a typo or an unsupported compose feature)"),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_unknown_reservations_device_key_via_recursion() {
	let msgs = warnings_for(
		"services:\n  db:\n    image: pg\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - capabilities: [gpu]\n              cont: 1\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("deploy.resources.reservations.devices[0]") && m.contains("cont")),
		"got: {msgs:?}"
	);
}

#[test]
fn warns_on_unknown_service_network_key() {
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    networks:\n      frontend:\n         alises: [a]\nnetworks:\n  frontend:\n",
	);
	assert!(
		msgs.iter().any(|m| m
			== "service 'web' networks.frontend: unknown key 'alises' is ignored (check for a typo or an unsupported compose feature)"),
		"got: {msgs:?}"
	);
}

// --- x- extension keys are never flagged -----------------------------------

#[test]
fn x_extension_key_in_a_block_is_not_flagged() {
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: bind\n        source: /host\n        target: /in\n        bind:\n          x-foo: bar\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("x-foo")),
		"x- extension keys must never be flagged; got: {msgs:?}"
	);
}

// --- Negative: modeled keys (incl. empty/null values) never warn -----------

#[test]
fn fully_modeled_blocks_produce_no_warning() {
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: bind\n        source: /host\n        target: /in\n        bind:\n          propagation: rprivate\n          create_host_path: true\n          selinux: z\n    networks:\n      frontend:\n        aliases: [web]\n        ipv4_address: 10.0.0.2\n    deploy:\n      resources:\n        limits:\n          cpus: '0.5'\n          memory: 512M\n          pids: 100\nnetworks:\n  frontend:\n",
	);
	assert!(msgs.is_empty(), "unexpected warnings: {msgs:?}");
}

#[test]
fn modeled_key_with_null_value_does_not_warn() {
	// `propagation:` is a modeled key whose value is null; a round-trip would
	// drop it (skip_serializing_if) and mis-flag it, but the allowlist must not.
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: bind\n        source: /host\n        target: /in\n        bind:\n          propagation:\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("propagation")),
		"a modeled-but-null key must not warn; got: {msgs:?}"
	);
}

#[test]
fn modeled_keys_with_empty_collections_do_not_warn() {
	// Empty modeled collections (`link_local_ips: []`, `driver_opts: {}` on a
	// service network and `devices: []` on a reservation) would all be dropped
	// by a round-trip; none may warn.
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    networks:\n      frontend:\n        aliases: []\n        link_local_ips: []\n        driver_opts: {}\n    deploy:\n      resources:\n        reservations:\n          devices: []\nnetworks:\n  frontend:\n",
	);
	assert!(msgs.is_empty(), "unexpected warnings: {msgs:?}");
}

#[test]
fn modeled_empty_options_in_driver_config_does_not_warn() {
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - type: volume\n        source: data\n        target: /data\n        volume:\n          driver_config:\n            name: local\n            options: {}\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("options")),
		"a modeled-but-empty map must not warn; got: {msgs:?}"
	);
}

#[test]
fn clean_file_produces_no_warning() {
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - ./data:/app/data\n",
	);
	assert!(msgs.is_empty(), "unexpected warnings: {msgs:?}");
}

#[test]
fn null_network_attachment_is_not_a_block() {
	// `networks: { frontend: }` is a null attachment, not an options map: there
	// is nothing to diff and it must not warn.
	let msgs = warnings_for(
		"services:\n  web:\n    image: nginx\n    networks:\n      frontend:\nnetworks:\n  frontend:\n",
	);
	assert!(msgs.is_empty(), "unexpected warnings: {msgs:?}");
}

#[test]
fn unparseable_document_yields_no_warnings() {
	assert!(raw_nested_unknown_warnings(": : :").is_empty());
}
