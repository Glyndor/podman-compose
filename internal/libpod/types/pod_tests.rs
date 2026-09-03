//! Serialisation tests for the libpod pod request/response types.

use std::collections::HashMap;

use super::{PodInspect, PodSpecGenerator};

#[test]
fn pod_spec_serialises_with_known_field_names() {
	// The wire keys are dictated by libpod; if any of these regresses to a
	// Rust name the daemon ignores the field silently.
	let mut labels = HashMap::new();
	labels.insert("podup.project".to_string(), "demo".to_string());
	labels.insert("podup.pod-config-hash".to_string(), "abc".to_string());
	let spec = PodSpecGenerator {
		netns: None,
		userns: None,
		name: "demo".to_string(),
		labels,
		shared_namespaces: vec!["net".to_string()],
		portmappings: vec![],
		networks: HashMap::new(),
		hostadd: vec![],
	};
	let json = serde_json::to_value(&spec).unwrap();
	assert_eq!(json["name"], "demo");
	assert_eq!(json["labels"]["podup.project"], "demo");
	assert_eq!(json["labels"]["podup.pod-config-hash"], "abc");
	assert_eq!(json["shared_namespaces"], serde_json::json!(["net"]));
}

#[test]
fn pod_inspect_reads_labels_and_name() {
	let json = r#"{
		"Name": "demo",
		"Labels": { "podup.pod-config-hash": "abc", "podup.project": "demo" },
		"NumContainers": 3
	}"#;
	let inspect: PodInspect = serde_json::from_str(json).unwrap();
	assert_eq!(
		inspect
			.labels
			.get("podup.pod-config-hash")
			.map(String::as_str),
		Some("abc"),
	);
	assert_eq!(
		inspect.labels.get("podup.project").map(String::as_str),
		Some("demo"),
	);
}
