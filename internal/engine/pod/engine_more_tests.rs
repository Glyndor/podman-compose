//! The second half of the pod engine tests, split from `engine_tests.rs`
//! to keep both under the repository line limit: the stale container list
//! after a recreate, the infra container, inspect failures, the label
//! sweep and the pod's user namespace.

use crate::compose::parse_str;
use crate::engine::fake_podman;

use super::engine_tests::{engine_for, pod_up_fake, route};

/// A pod recreate removes its member containers with it. The container list
/// `up` fetched before that is stale afterwards; without forgetting it, `up`
/// reads the listed container as unchanged and only starts it, which is a
/// start on a container that no longer exists.
#[tokio::test]
#[cfg(unix)]
async fn a_recreated_pod_forgets_the_containers_it_removed() {
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
"#;
	let file = parse_str(yaml).unwrap();
	// The listed container carries the hash and image ID `up` will compute, so
	// with a stale list it reads as unchanged.
	let hash = crate::engine::container::config_hash(&file.services["web"], &file).unwrap();
	let listing = format!(
		r#"[{{"Id":"aaa","Names":["/proj-web-1"],"Image":"nginx","ImageID":"sha256:0000000000000000000000000000000000000000000000000000000000000000","Status":"","State":"running","Ports":[],"Labels":{{"podup.project":"proj","podup.service":"web","podup.config-hash":"{hash}"}}}}]"#
	);
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			return (200, listing.clone());
		}
		route(method, target, Some("wrong-hash"))
	});
	let engine = engine_for(&fake, "proj");
	engine.up(&file).await.expect("up must succeed");

	let requests = fake.requests.lock().unwrap().clone();
	let recreated_pod = requests
		.iter()
		.any(|r| r.starts_with("DELETE") && r.contains("/pods/proj") && r.contains("force=true"));
	let created_container = requests
		.iter()
		.any(|r| r.starts_with("POST") && r.contains("/containers/create"));
	assert!(
		recreated_pod,
		"the mismatching pod must be recreated; requests: {requests:?}"
	);
	assert!(
		created_container,
		"the service must be created afresh, not started as if it survived the pod; requests: {requests:?}"
	);
}

/// The infra container carries the project label and belongs to no service.
/// On the first real run it was reported, and would have been removed, as an
/// orphan; it lives and dies with the pod.
#[tokio::test]
#[cfg(unix)]
async fn the_infra_container_is_not_an_orphan() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			return (
				200,
				r#"[{"Id":"iii","Names":["/abc-infra"],"Image":"localhost/podman-pause:5","ImageID":"sha256:pause","Status":"","State":"running","Ports":[],"IsInfra":true,"Labels":{"podup.project":"proj"}},{"Id":"sss","Names":["/proj-old-1"],"Image":"nginx","ImageID":"sha256:web","Status":"","State":"running","Ports":[],"Labels":{"podup.project":"proj","podup.service":"old"}}]"#.to_string(),
			);
		}
		route(method, target, None)
	});
	let engine = engine_for(&fake, "proj");
	let file = parse_str("x-podman-pod: true\nservices:\n  web:\n    image: nginx\n").unwrap();
	engine
		.remove_orphans(&file)
		.await
		.expect("remove_orphans must succeed");

	let requests = fake.requests.lock().unwrap().clone();
	assert!(
		requests
			.iter()
			.any(|r| r.starts_with("DELETE") && r.contains("/containers/proj-old-1")),
		"a container of a service no longer in the file is an orphan; requests: {requests:?}"
	);
	assert!(
		!requests
			.iter()
			.any(|r| r.starts_with("DELETE") && r.contains("infra")),
		"the infra container must never be removed as an orphan; requests: {requests:?}"
	);
}

/// Only a 404 on the pod inspect means "no pod yet". Any other failure must
/// surface, not be read as absence and answered with a create.
#[tokio::test]
#[cfg(unix)]
async fn a_failing_pod_inspect_is_an_error_not_an_absence() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/pods/") && target.contains("/json") {
			return (500, r#"{"message":"boom"}"#.to_string());
		}
		route(method, target, None)
	});
	let engine = engine_for(&fake, "proj");
	let file = parse_str("x-podman-pod: true\nservices:\n  web:\n    image: nginx\n").unwrap();
	let err = engine
		.up(&file)
		.await
		.expect_err("a 500 on inspect must fail up");
	assert!(
		format!("{err}").contains("boom"),
		"the engine's message must reach the user: {err}"
	);
	let requests = fake.requests.lock().unwrap().clone();
	assert!(
		!requests.iter().any(|r| r.contains("/pods/create")),
		"no pod may be created on top of an unreadable one; requests: {requests:?}"
	);
}

/// `down --remove-orphans` sweeps pods under the project's label that are
/// not the project's own pod, and leaves that one to `remove_pod`.
#[tokio::test]
#[cfg(unix)]
async fn the_label_sweep_removes_stale_pods_and_keeps_the_projects_own() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/pods/json") {
			return (200, r#"[{"Name":"proj"},{"Name":"proj-old"}]"#.to_string());
		}
		if method == "DELETE" && target.contains("/pods/") {
			return (200, r#"{"Id":"x"}"#.to_string());
		}
		route(method, target, None)
	});
	let engine = engine_for(&fake, "proj");
	engine.remove_project_pods_by_label().await;
	let requests = fake.requests.lock().unwrap().clone();
	assert!(
		requests
			.iter()
			.any(|r| r.starts_with("DELETE") && r.contains("/pods/proj-old")),
		"the stale pod under the label must be removed; requests: {requests:?}"
	);
	assert!(
		!requests
			.iter()
			.any(|r| r.starts_with("DELETE") && r.contains("/pods/proj?")),
		"the project's own pod is not the sweep's to remove; requests: {requests:?}"
	);
}

/// A common `userns_mode` lands on the pod, not on the members, and it is
/// part of the hash that decides a recreate.
#[tokio::test]
#[cfg(unix)]
async fn a_common_userns_mode_is_the_pods_and_not_the_members() {
	let fake = pod_up_fake();
	let engine = engine_for(&fake, "proj");
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
    userns_mode: auto
  db:
    image: postgres
    userns_mode: auto
"#;
	let file = parse_str(yaml).unwrap();
	engine.up(&file).await.expect("up must succeed");
	let requests = fake.requests.lock().unwrap().clone();
	let bodies = fake.bodies.lock().unwrap().clone();
	let mut pod_body = None;
	let mut container_bodies = Vec::new();
	for (r, b) in requests.iter().zip(bodies.iter()) {
		if r.contains("/pods/create") {
			pod_body = Some(serde_json::from_slice::<serde_json::Value>(b).unwrap());
		} else if r.contains("/containers/create") {
			container_bodies.push(serde_json::from_slice::<serde_json::Value>(b).unwrap());
		}
	}
	let pod_body = pod_body.expect("a pod must be created");
	assert_eq!(
		pod_body["userns"],
		serde_json::json!({"nsmode": "auto"}),
		"{pod_body}"
	);
	assert_eq!(container_bodies.len(), 2);
	for c in &container_bodies {
		assert!(
			c.get("userns").is_none() || c["userns"].is_null(),
			"a member must not carry its own userns: {c}"
		);
	}
	let without = parse_str(
		"x-podman-pod: true\nservices:\n  web:\n    image: nginx\n  db:\n    image: postgres\n",
	)
	.unwrap();
	let ports: Vec<Vec<crate::ports::ParsedPort>> = vec![Vec::new(), Vec::new()];
	assert_ne!(
		crate::engine::pod::pod_config_hash(&ports, &file),
		crate::engine::pod::pod_config_hash(&ports, &without),
		"the user namespace is part of the pod hash"
	);
}
