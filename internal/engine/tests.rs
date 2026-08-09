use super::*;
use crate::libpod::Client;

fn engine(project: &str) -> Engine {
	Engine::with_base_dir(
		Client::new("/nonexistent.sock"),
		project.into(),
		std::env::temp_dir(),
	)
}

fn scaled_service(replicas: u32) -> Service {
	Service {
		scale: Some(replicas),
		..Service::default()
	}
}

#[test]
fn replica_names_always_index_suffix_default_name() {
	// The #815 contract: an auto-generated container name is ALWAYS
	// index-suffixed, even for a single replica (docker/podman parity).
	let e = engine("proj");
	assert_eq!(
		e.replica_names("web", &Service::default()),
		vec!["proj-web-1".to_string()]
	);
	assert_eq!(
		e.replica_names("web", &scaled_service(3)),
		vec![
			"proj-web-1".to_string(),
			"proj-web-2".to_string(),
			"proj-web-3".to_string(),
		]
	);
}

#[test]
fn replica_names_honour_explicit_container_name_verbatim() {
	// An explicit `container_name:` is the user's exact choice and is never
	// index-suffixed at a single replica.
	let e = engine("proj");
	let svc = Service {
		container_name: Some("my-db".to_string()),
		..Service::default()
	};
	assert_eq!(e.replica_names("db", &svc), vec!["my-db".to_string()]);
	assert_eq!(e.first_replica_name("db", &svc), "my-db");
}

#[test]
fn replica_names_for_zero_scale_is_empty() {
	// `--scale svc=0` resolves to no containers, so the name set is empty.
	let e = engine("proj");
	assert!(e
		.replica_names_for("web", &Service::default(), 0)
		.is_empty());
}

#[test]
fn replica_name_at_index_zero_is_rejected() {
	// `--index` is 1-based; index 0 must be an error, never replica 1.
	let e = engine("proj");
	let svc = scaled_service(3);
	let err = e
		.replica_name_at("web", &svc, Some(0))
		.expect_err("index 0 must be rejected");
	assert!(
		matches!(err, ComposeError::ReplicaIndex { index: 0, ref service } if service == "web"),
		"unexpected error: {err:?}"
	);
	// The index hint renders outside the quoted service name.
	let msg = err.to_string();
	assert!(
		msg.contains("'web'") && msg.contains("1-based"),
		"got {msg:?}"
	);
}

#[test]
fn replica_name_at_index_one_is_first_replica() {
	let e = engine("proj");
	let svc = scaled_service(3);
	assert_eq!(
		e.replica_name_at("web", &svc, Some(1)).unwrap(),
		"proj-web-1"
	);
}

#[test]
fn replica_name_at_index_n_is_nth_replica() {
	let e = engine("proj");
	let svc = scaled_service(3);
	assert_eq!(
		e.replica_name_at("web", &svc, Some(3)).unwrap(),
		"proj-web-3"
	);
}

#[test]
fn replica_name_at_out_of_range_is_rejected() {
	let e = engine("proj");
	let svc = scaled_service(3);
	assert!(e.replica_name_at("web", &svc, Some(4)).is_err());
}

#[test]
fn replica_name_at_none_is_first_replica() {
	let e = engine("proj");
	// Single replica: the first index-suffixed name (always-suffix parity
	// with docker/podman — there is no bare, unnumbered container).
	assert_eq!(
		e.replica_name_at("web", &Service::default(), None).unwrap(),
		"proj-web-1"
	);
	// Multiple replicas: the first suffixed name.
	assert_eq!(
		e.replica_name_at("web", &scaled_service(3), None).unwrap(),
		"proj-web-1"
	);
}

fn names(list: &[&str]) -> Vec<String> {
	list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn order_replicas_sorts_by_replica_number() {
	let live = names(&["proj-web-10", "proj-web-2", "proj-web-1"]);
	assert_eq!(
		super::replicas::order_replicas("proj-web", &live),
		names(&["proj-web-1", "proj-web-2", "proj-web-10"])
	);
}

// `project_label_filter_*` (#1364)

/// The cached `{"label":["podup.project=<name>"]}` JSON for container
/// listings is built once per `Engine` and reused across every
/// container-list call site. The two halves of the cache agree on the
/// project name and on the URL-encoded JSON shape.
#[test]
fn project_label_cache_matches_handbuilt_filter() {
	let e = engine("demo");
	let expected = crate::libpod::urlencoded(
		&serde_json::json!({ "label": ["podup.project=demo"] }).to_string(),
	);
	assert_eq!(e.project_label_filter_encoded(), expected);
	// The raw label is the splice point for the dynamic sites.
	assert_eq!(e.project_label_raw(), "podup.project=demo");
}

/// The container and network cache halves are the same JSON, so the
/// network-side call site can reuse the same encoding (#1364).
#[test]
fn project_label_cache_container_and_network_match() {
	let e = engine("demo");
	assert_eq!(
		e.project_label_filter_encoded(),
		e.project_network_filter_encoded(),
	);
}

/// The dynamic filter (one extra label) splices the project label once and
/// re-encodes only once, matching the hand-built filter for the same labels.
#[test]
fn project_label_filter_with_splices_project_once() {
	let e = engine("demo");
	let with = e.project_label_filter_with(["podup.service=web".to_string()]);
	let expected = crate::libpod::urlencoded(
		&serde_json::json!({
			"label": ["podup.project=demo", "podup.service=web"],
		})
		.to_string(),
	);
	assert_eq!(with, expected);
}
