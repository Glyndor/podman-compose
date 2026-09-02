use super::*;
use crate::libpod::types::container::spec::Namespace;

// ---------------------------------------------------------------------------
// Namespace (spec type — tested here to avoid pushing spec.rs over 500 lines)
// ---------------------------------------------------------------------------

#[test]
fn namespace_new_has_no_value() {
	let ns = Namespace::new("host");
	assert_eq!(ns.nsmode, "host");
	assert!(ns.value.is_none());
}

#[test]
fn namespace_container_sets_value() {
	let ns = Namespace::container("other");
	assert_eq!(ns.nsmode, "container");
	assert_eq!(ns.value.as_deref(), Some("other"));
}

#[test]
fn namespace_parse_container_prefix() {
	let ns = Namespace::parse("container:sidecar");
	assert_eq!(ns.nsmode, "container");
	assert_eq!(ns.value.as_deref(), Some("sidecar"));
}

#[test]
fn namespace_parse_plain_mode() {
	let ns = Namespace::parse("host");
	assert_eq!(ns.nsmode, "host");
	assert!(ns.value.is_none());
}

// ---------------------------------------------------------------------------
// Response deserialization
// ---------------------------------------------------------------------------

#[test]
fn container_inspect_deserialize_healthy() {
	let json = r#"{
		"State": {
			"Status": "running",
			"ExitCode": 0,
			"Health": { "Status": "healthy" }
		}
	}"#;
	let ci: ContainerInspect = serde_json::from_str(json).unwrap();
	let state = ci.state.unwrap();
	assert_eq!(state.status.as_deref(), Some("running"));
	assert_eq!(state.exit_code, Some(0));
	assert_eq!(state.health.unwrap().status.as_deref(), Some("healthy"));
}

#[test]
fn container_inspect_missing_fields_default() {
	let json = r#"{}"#;
	let ci: ContainerInspect = serde_json::from_str(json).unwrap();
	assert!(ci.state.is_none());
	assert!(ci.config.is_none());
	assert!(ci.network_settings.is_none());
}

#[test]
fn has_healthcheck_true_for_image_inherited() {
	let json = r#"{
		"Config": { "Healthcheck": { "Test": ["CMD-SHELL", "curl -f http://localhost || exit 1"] } }
	}"#;
	let ci: ContainerInspect = serde_json::from_str(json).unwrap();
	assert!(ci.config.unwrap().has_healthcheck());
}

#[test]
fn has_healthcheck_false_when_disabled_with_none() {
	let json = r#"{ "Config": { "Healthcheck": { "Test": ["NONE"] } } }"#;
	let ci: ContainerInspect = serde_json::from_str(json).unwrap();
	assert!(!ci.config.unwrap().has_healthcheck());
}

#[test]
fn has_healthcheck_false_when_absent() {
	let json = r#"{ "Config": {} }"#;
	let ci: ContainerInspect = serde_json::from_str(json).unwrap();
	assert!(!ci.config.unwrap().has_healthcheck());
}

#[test]
fn has_healthcheck_false_when_test_null() {
	let json = r#"{ "Config": { "Healthcheck": { "Test": null } } }"#;
	let ci: ContainerInspect = serde_json::from_str(json).unwrap();
	assert!(!ci.config.unwrap().has_healthcheck());
}

#[test]
fn top_response_deserialize() {
	let json = r#"{"Titles": ["PID", "CMD"], "Processes": [["1", "bash"]]}"#;
	let tr: TopResponse = serde_json::from_str(json).unwrap();
	assert_eq!(tr.titles.unwrap(), vec!["PID", "CMD"]);
	assert_eq!(tr.processes.unwrap(), vec![vec!["1", "bash"]]);
}

#[test]
fn container_list_entry_default_fields() {
	let json = r#"{"Names": ["/mycontainer"], "Image": "nginx", "Status": "running", "Ports": []}"#;
	let entry: ContainerListEntry = serde_json::from_str(json).unwrap();
	assert_eq!(entry.names, vec!["/mycontainer"]);
	assert_eq!(entry.image, "nginx");
	assert_eq!(entry.status, "running");
}

#[test]
fn container_list_entry_null_vec_fields() {
	let json = r#"{"Names": null, "Image": "alpine", "Status": "exited", "Ports": null}"#;
	let entry: ContainerListEntry = serde_json::from_str(json).unwrap();
	assert!(entry.names.is_empty());
	assert!(entry.ports.is_empty());
}
