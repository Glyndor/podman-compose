//! Unit tests for the `x-podman-pod` engine wiring against the fake Podman
//! socket. Each test stands up a project with the extension set, runs `up`
//! or `down`, and asserts on the wire requests / responses the engine
//! produced. The shared fake-socket helper is below; each test that needs a
//! richer routing rule builds its own.

use crate::compose::parse_str;
use crate::engine::fake_podman;
use crate::engine::Engine;

/// Fake socket answering the requests an `up`/`down` pass makes for a
/// pod-enabled project. Every request gets a 200 with the smallest body the
/// daemon would return; the test then inspects `fake.requests` and
/// `fake.bodies` to assert what the engine actually did.
fn pod_up_fake() -> fake_podman::FakePodman {
	fake_podman::start(|method, target| route(method, target, None))
}

/// As [`pod_up_fake`] but reports the pod as already existing with a given
/// hash label. The engine will inspect it, compare against the new hash,
/// and either reuse or recreate.
fn pod_up_fake_with_existing(existing_hash: Option<&str>) -> fake_podman::FakePodman {
	let hash_label = existing_hash.unwrap_or("wrong-hash").to_string();
	fake_podman::start(move |method, target| route(method, target, Some(&hash_label)))
}

/// One fake routing table, parameterised by the recorded `podup.pod-config-hash`.
/// `existing_hash == None` means the pod does not yet exist (the engine
/// creates it); `Some(hash)` means the pod exists with that recorded hash.
fn route(method: &str, target: &str, existing_hash: Option<&str>) -> (u16, String) {
	if method == "GET" && target.contains("/containers/json") {
		(200, "[]".to_string())
	} else if method == "POST"
		&& (target.contains("/networks/create") || target.contains("/volumes/create"))
	{
		(200, r#"{}"#.to_string())
	} else if method == "POST" && target.contains("/secrets/create") {
		(200, r#"{"ID":"secret-id"}"#.to_string())
	} else if method == "POST" && target.contains("/containers/create") {
		(200, r#"{"Id":"abc","Warnings":[]}"#.to_string())
	} else if method == "POST" && target.contains("/containers/") && target.contains("/wait") {
		(200, r#"{"StatusCode":0}"#.to_string())
	} else if method == "POST" && (target.contains("/start") || target.contains("/images/pull")) {
		(200, String::new())
	} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
		(
			200,
			r#"{"Id":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}"#
				.to_string(),
		)
	} else if method == "DELETE" && target.contains("/containers/") {
		(404, r#"{"message":"no such container"}"#.to_string())
	} else if method == "POST" && target.contains("/pods/create") {
		(200, r#"{"Id":"pod-id"}"#.to_string())
	} else if method == "GET" && target.contains("/pods/") && target.contains("/json") {
		if existing_hash.is_none() {
			// Pod does not exist.
			(404, r#"{"message":"no such pod"}"#.to_string())
		} else {
			let hash_label = existing_hash.unwrap_or("wrong-hash");
			(
				200,
				format!(
					r#"{{"Name":"proj","Labels":{{"podup.pod-config-hash":"{hash_label}"}},"NumContainers":2}}"#
				),
			)
		}
	} else if method == "GET" && target.contains("/pods/") && target.contains("/exists") {
		let hash_label = existing_hash.unwrap_or("wrong-hash");
		(
			200,
			format!(
				r#"{{"Name":"proj","Labels":{{"podup.pod-config-hash":"{hash_label}"}},"NumContainers":2}}"#
			),
		)
	} else if method == "DELETE" && target.contains("/pods/") {
		(200, String::new())
	} else if method == "GET" && target.contains("/networks/json") {
		(200, "[]".to_string())
	} else if method == "GET" && target.contains("/secrets/") && target.contains("/json") {
		// External-secret inspect: pretend every external secret exists.
		(200, r#"{"ID":"ext","Spec":{"Labels":{}}}"#.to_string())
	} else if method == "DELETE" && target.contains("/secrets/") {
		(200, String::new())
	} else {
		(404, r#"{"message":"unexpected"}"#.to_string())
	}
}

/// Decode the JSON body of one of the requests the engine sent. The
/// `SpecGenerator` is the largest payload and is the one most tests care
/// about; `PodSpecGenerator` is the second.
fn decode_body(bodies: &[Vec<u8>], needle: &str) -> serde_json::Value {
	for (i, b) in bodies.iter().enumerate() {
		if b.is_empty() {
			continue;
		}
		if let Ok(v) = serde_json::from_slice::<serde_json::Value>(b) {
			// Cheap discriminator: every other body in these tests is an
			// empty JSON object/array. The `pod` and `portmappings` fields
			// only appear on the bodies we care about.
			if needle == "pod" && v.get("pod").is_some() {
				let _ = i;
				return v;
			}
			if needle == "pods" && v.get("shared_namespaces").is_some() {
				return v;
			}
		}
	}
	panic!("no body matching {needle} in {} bodies", bodies.len())
}

fn engine_for(fake: &fake_podman::FakePodman, project: &str) -> Engine {
	Engine::with_base_dir(fake.client(), project.to_string(), std::env::temp_dir())
}

/// Pod create body carries the project name, the two `podup.*` labels, the
/// shared `net` namespace, the union of every service's port set, the
/// declared networks, and one `hostadd` per service.
#[tokio::test]
#[cfg(unix)]
async fn pod_create_body_carries_name_labels_ports_networks_and_hosts() {
	let fake = pod_up_fake();
	let engine = engine_for(&fake, "proj");
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
    ports: ["8080:80"]
  db:
    image: postgres
    ports: ["5432:5432"]
networks:
  backend:
"#;
	let file = parse_str(yaml).expect("compose must parse");
	engine.up(&file).await.expect("up must succeed");

	let bodies = fake.bodies.lock().unwrap().clone();
	let pod_body = decode_body(&bodies, "pods");

	assert_eq!(pod_body["name"], "proj");
	assert_eq!(pod_body["labels"]["podup.project"], "proj");
	assert!(
		pod_body["labels"]["podup.pod-config-hash"]
			.as_str()
			.map(|s| !s.is_empty())
			.unwrap_or(false),
		"pod must carry a pod-config-hash label"
	);
	assert_eq!(pod_body["shared_namespaces"], serde_json::json!(["net"]));
	// A pod that attaches to networks must ask for bridge networking, or libpod refuses it.
	assert_eq!(pod_body["netns"], serde_json::json!({"nsmode": "bridge"}));
	assert!(
		pod_body["networks"].get("proj_backend").is_some(),
		"the pod must attach to the declared network: {pod_body}"
	);
	// Union of every service's port set: 80, 5432 container ports; 8080, 5432 host ports.
	let ports = pod_body["portmappings"]
		.as_array()
		.expect("portmappings must be an array");
	let container_ports: Vec<u16> = ports
		.iter()
		.map(|p| p["container_port"].as_u64().unwrap() as u16)
		.collect();
	assert!(container_ports.contains(&80));
	assert!(container_ports.contains(&5432));
	// Host entries: one per service.
	let hosts: Vec<String> = pod_body["hostadd"]
		.as_array()
		.expect("hostadd must be an array")
		.iter()
		.map(|v| v.as_str().unwrap().to_string())
		.collect();
	assert!(hosts.contains(&"web:127.0.0.1".to_string()));
	assert!(hosts.contains(&"db:127.0.0.1".to_string()));
}

/// Containers created in pod mode carry `pod` set to the project name and
/// have no per-container portmappings, networks, or netns.
#[tokio::test]
#[cfg(unix)]
async fn pod_containers_carry_pod_and_no_ports_or_networks() {
	let fake = pod_up_fake();
	let engine = engine_for(&fake, "proj");
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
    ports: ["8080:80"]
  db:
    image: postgres
"#;
	let file = parse_str(yaml).expect("compose must parse");
	engine.up(&file).await.expect("up must succeed");

	let bodies = fake.bodies.lock().unwrap().clone();
	// Every container create body should carry `pod` and an empty
	// portmappings/networks. Pick any one (the first non-empty body).
	let pod_body = decode_body(&bodies, "pod");
	assert_eq!(pod_body["pod"], "proj");
	// `portmappings`, `networks` and `netns` carry
	// `skip_serializing_if = ...is_empty/is_none`, so an empty list is
	// omitted from the JSON body rather than serialised as `[]`/`{}`.
	assert!(
		pod_body.get("portmappings").is_none() || pod_body["portmappings"].is_null(),
		"container inside a pod cannot publish ports, got: {pod_body}"
	);
	assert!(
		pod_body.get("networks").is_none() || pod_body["networks"].is_null(),
		"container inside a pod cannot attach networks, got: {pod_body}"
	);
	assert!(
		pod_body.get("netns").is_none() || pod_body["netns"].is_null(),
		"container inside a pod must not carry its own netns, got: {pod_body}"
	);
}

/// Adding a port changes the pod's hash; changing the service's command does
/// not.
#[tokio::test]
#[cfg(unix)]
async fn pod_hash_changes_with_a_port_and_not_with_a_command() {
	let yaml_base = r#"
x-podman-pod: true
services:
  web:
    image: nginx
"#;
	let file_base = parse_str(yaml_base).unwrap();
	let base_ports = vec![crate::ports::parse_ports(&file_base.services["web"].ports).unwrap()];

	let yaml_with_port = r#"
x-podman-pod: true
services:
  web:
    image: nginx
    ports: ["8080:80"]
"#;
	let file_with_port = parse_str(yaml_with_port).unwrap();
	let with_port_ports =
		vec![crate::ports::parse_ports(&file_with_port.services["web"].ports).unwrap()];

	let yaml_with_command = r#"
x-podman-pod: true
services:
  web:
    image: nginx
    command: ["nginx", "-g", "daemon off;"]
"#;
	let file_with_command = parse_str(yaml_with_command).unwrap();
	let with_command_ports =
		vec![crate::ports::parse_ports(&file_with_command.services["web"].ports).unwrap()];

	let base = crate::engine::pod::pod_config_hash(&base_ports, &file_base);
	let with_port = crate::engine::pod::pod_config_hash(&with_port_ports, &file_with_port);
	let with_command = crate::engine::pod::pod_config_hash(&with_command_ports, &file_with_command);

	assert_ne!(base, with_port, "adding a port must change the pod hash");
	assert_eq!(
		base, with_command,
		"changing the service command must NOT change the pod hash"
	);
}

/// When the recorded hash differs from the new one, the pod is recreated:
/// `DELETE /pods/{name}?force=true` then `POST /pods/create`.
#[tokio::test]
#[cfg(unix)]
async fn pod_is_recreated_when_the_hash_differs() {
	let fake = pod_up_fake_with_existing(Some("wrong-hash"));
	let engine = engine_for(&fake, "proj");
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
"#;
	let file = parse_str(yaml).unwrap();
	engine.up(&file).await.expect("up must succeed");

	let requests = fake.requests.lock().unwrap().clone();
	let deleted_pod = requests
		.iter()
		.any(|r| r.starts_with("DELETE") && r.contains("/pods/proj") && r.contains("force=true"));
	let created_pod = requests
		.iter()
		.any(|r| r.starts_with("POST") && r.contains("/pods/create"));
	assert!(
		deleted_pod && created_pod,
		"a pod with a mismatching hash must be removed and recreated; requests: {requests:?}"
	);
}

/// When the recorded hash matches, the engine does NOT re-create the pod.
#[tokio::test]
#[cfg(unix)]
async fn pod_is_reused_when_the_hash_matches() {
	// Compute the expected hash for this compose file and feed it back as
	// the recorded hash so the engine reuses the pod.
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
    ports: ["8080:80"]
"#;
	let file = parse_str(yaml).unwrap();
	let expected = crate::engine::pod::pod_config_hash(
		&[crate::ports::parse_ports(&file.services["web"].ports).unwrap()],
		&file,
	);
	let fake = pod_up_fake_with_existing(Some(&expected));
	let engine = engine_for(&fake, "proj");
	engine.up(&file).await.expect("up must succeed");

	let requests = fake.requests.lock().unwrap().clone();
	let deleted_pod = requests
		.iter()
		.any(|r| r.starts_with("DELETE") && r.contains("/pods/proj") && r.contains("force=true"));
	let created_pod = requests
		.iter()
		.filter(|r| r.starts_with("POST") && r.contains("/pods/create"))
		.count();
	let inspected_pod = requests
		.iter()
		.filter(|r| r.starts_with("GET") && r.contains("/pods/") && r.contains("/json"))
		.count();
	assert!(
		!deleted_pod,
		"a matching-hash pod must not be removed; requests: {requests:?}"
	);
	assert_eq!(
		created_pod, 0,
		"a matching-hash pod must NOT be created again, but a recreate would also POST; requests: {requests:?}"
	);
	assert!(
		inspected_pod >= 1,
		"the engine must inspect the pod to read its hash before deciding; requests: {requests:?}"
	);
}

/// `down` removes the project's containers and THEN the pod.
#[tokio::test]
#[cfg(unix)]
async fn down_removes_the_pod_after_the_containers() {
	let fake = pod_up_fake();
	let engine = engine_for(&fake, "proj");
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
"#;
	let file = parse_str(yaml).unwrap();
	engine.up(&file).await.expect("up must succeed");
	engine.down(&file).await.expect("down must succeed");

	let requests = fake.requests.lock().unwrap().clone();
	let pod_del_pos = requests
		.iter()
		.position(|r| r.starts_with("DELETE") && r.contains("/pods/proj"))
		.expect("down must DELETE the pod");
	let container_del_positions: Vec<usize> = requests
		.iter()
		.enumerate()
		.filter(|(_, r)| r.starts_with("DELETE") && r.contains("/containers/"))
		.map(|(i, _)| i)
		.collect();
	assert!(
		!container_del_positions.is_empty(),
		"down must delete at least one container; requests: {requests:?}"
	);
	assert!(
		container_del_positions.iter().all(|p| *p < pod_del_pos),
		"every container DELETE must happen before the pod DELETE; requests: {requests:?}"
	);
}

/// `ps` hides the infra container (`IsInfra: true` on the list response).
#[tokio::test]
#[cfg(unix)]
async fn ps_hides_the_infra_container() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			// Two containers: one regular, one infra.
			(
				200,
				r#"[
					{"Id":"aaa","Names":["/proj-web-1"],"Image":"nginx","ImageID":"sha256:web","Status":"","State":"running","Ports":[],"Labels":{"podup.project":"proj","podup.service":"web"},"Created":"2026-01-01T00:00:00Z","IsInfra":false},
					{"Id":"bbb","Names":["/proj-infra"],"Image":"k8s.gcr.io/pause:3.5","ImageID":"sha256:pause","Status":"","State":"running","Ports":[],"Labels":{"podup.project":"proj"},"Created":"2026-01-01T00:00:00Z","IsInfra":true}
				]"#
					.to_string(),
			)
		} else {
			(404, r#"{"message":"unexpected"}"#.to_string())
		}
	});
	let engine = engine_for(&fake, "proj");
	let file = parse_str("services:\n  web:\n    image: nginx\n").unwrap();
	// Use the test-only row helper so the test can introspect the rows
	// directly without going through stdout.
	let json_rows = crate::engine::query::ps_rows_for_test(&engine, &file)
		.await
		.expect("ps must succeed");
	let names: Vec<&str> = json_rows
		.iter()
		.map(|r| r["Name"].as_str().unwrap_or(""))
		.collect();
	assert!(
		names.contains(&"proj-web-1"),
		"regular container must be listed: {names:?}"
	);
	assert!(
		!names.iter().any(|n| n.contains("infra")),
		"infra container must be hidden from ps; got: {names:?}"
	);
}

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
