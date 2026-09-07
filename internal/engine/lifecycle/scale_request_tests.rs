//! Request-counting cases split out of `scale_tests.rs`, which reached the
//! 500-line hard limit. These are the ones that stand up a fake libpod
//! socket and assert how many round-trips a command makes (#1363, #1445,
//! #1742); the rest of the file is pure reconciliation logic with no
//! socket. The split follows that seam rather than a line count.

use super::*;

// Every case in this file stands up a fake libpod socket, and the fixture
// binds a `UnixListener`, so the whole file is Unix-only. The imports and
// the helper carry the same `cfg` as the cases rather than the module
// carrying one, which is the shape `scale_tests.rs` already uses.
#[cfg(unix)]
use crate::engine::fake_podman;

#[cfg(unix)]
fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

/// #1445 is a round-trip count, so pin the count rather than only the values it
/// produces. Before it, every selected service cost its own `/containers/json`
/// GET; a project of N services made N of them. Nothing in the value assertions
/// above notices if that regresses (the names come back identical either way)
/// so a later refactor could quietly put the call back inside the loop.
///
/// Four services, and the fake records every request it answers. The assertion
/// is that exactly one container-list GET reaches the socket.
#[tokio::test]
#[cfg(unix)]
async fn logs_issues_one_container_list_for_the_whole_project() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"State":"running","Labels":{"podup.service":"web"}},
		{"Names":["/proj-api-1"],"State":"running","Labels":{"podup.service":"api"}},
		{"Names":["/proj-db-1"],"State":"running","Labels":{"podup.service":"db"}},
		{"Names":["/proj-cache-1"],"State":"running","Labels":{"podup.service":"cache"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else {
			// Every per-container logs stream 404s: this test is about how many
			// listing calls are made, not about what the streams carry.
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let file = crate::parse_str(
		"services:\n  web:\n    image: x\n  api:\n    image: x\n  db:\n    image: x\n  cache:\n    image: x\n",
	)
	.unwrap();

	// The per-container streams all 404, so the call itself is expected to
	// fail; the request log is what carries the answer.
	let _ = e.logs(&file, None, false).await;

	let lists = fake
		.requests
		.lock()
		.unwrap()
		.iter()
		.filter(|r| r.contains("/containers/json"))
		.count();
	assert_eq!(
		lists,
		1,
		"four services must share one container-list round-trip, not one each (#1445); \
		 requests were {:?}",
		fake.requests.lock().unwrap()
	);
}

/// A surplus replica's row opens with `Stopping` before the stop request,
/// moves to `Removing` before the delete, and closes with `Removed`. The row
/// used to appear only at `Removed`, with no start time and nothing on screen
/// during a ten-second grace (#1686).
#[tokio::test]
#[cfg(unix)]
async fn stop_and_remove_opens_the_row_before_the_stop_and_closes_it_removed() {
	let live_snap = r#"[{"Names":["/proj-web-1"]},{"Names":["/proj-web-2"]}]"#;
	// Pre-build the snapshot the production `run_up` hands the function
	// (no `/containers/json` here either; the fix takes the bulk snapshot
	// rather than refetching per-service, see `#1747`).
	let mut existing: std::collections::HashMap<String, super::ExistingContainer> =
		std::collections::HashMap::new();
	for raw in ["proj-web-1", "proj-web-2"] {
		existing.insert(
			raw.to_string(),
			super::ExistingContainer {
				config_hash: None,
				image_id: String::new(),
				service: Some("web".to_string()),
			},
		);
	}
	let fake = fake_podman::start(move |method, target| {
		let _ = live_snap; // keep the literal in the closure for parity
		if (method == "POST" && target.contains("/stop"))
			|| (method == "DELETE" && target.contains("/proj-web-2?force=true"))
		{
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let capture = crate::ui::progress::capture::Capture::start();
	e.remove_surplus_replicas(
		"web",
		&crate::compose::types::Service::default(),
		1,
		&existing,
	)
	.await
	.expect("the surplus replica is removed");
	let verbs: Vec<String> = capture
		.verbs()
		.into_iter()
		.filter(|(_, name, _)| name == "proj-web-2")
		.map(|(_, _, verb)| verb)
		.collect();
	assert_eq!(
		verbs,
		vec!["Stopping", "Removing", "Removed"],
		"the row is opened before the stop and closed after the delete"
	);
}

/// #1747 (L4): the `up` walk with at least one `--scale` override used to
/// issue one project container-list GET at the top of `run_up` and then a
/// second GET per scale override inside `remove_surplus_replicas`, both for
/// the same `podup.project` filter shape (the second call added a
/// `podup.service=` predicate on top). With two overrides, that was three
/// `/containers/json` round trips against data that was already on hand
/// after the first GET. The fix pushes the bulk snapshot through
/// `remove_surplus_replicas`; the assertion here pins the round-trip count
/// to one for the bulk fetch alone, regardless of override count.
#[tokio::test]
#[cfg(unix)]
async fn up_with_a_scale_override_issues_one_project_list_get() {
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::sync::Arc;
	let list_calls = Arc::new(AtomicUsize::new(0));
	let list_calls_for_closure = list_calls.clone();
	// Every `/containers/json` is the bulk project list; every
	// `/containers/<n>/stop` or `/containers/<n>?force=true` is the
	// per-replica reconcile work `remove_surplus_replicas` does on top of
	// it. The marker counts only the first kind: the fix's contract is
	// that this is fetched *once* per `up`.
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			list_calls_for_closure.fetch_add(1, Ordering::Relaxed);
			(
				200,
				r#"[{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}}]"#.to_string(),
			)
		} else if method == "POST" && target.contains("/stop")
			|| method == "DELETE" && target.contains("/proj-web-1?force=true")
		{
			(404, r#"{"message":"no such container"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let file = crate::parse_str("services:\n  web:\n    image: x\n").unwrap();
	let engine = e.with_scale_overrides(std::collections::HashMap::from([("web".to_string(), 2)]));
	// `up` will fail later (image-create paths 404, etc.); what carries the
	// assertion is the recorded GET count and the projected request log.
	let _ = engine.up(&file).await;

	let lists = list_calls.load(Ordering::Relaxed);
	assert_eq!(
		lists,
		1,
		"`up --scale` must issue one `/containers/json` fetch (the bulk snapshot), \
		 not one plus another per `--scale` override; requests were {:?}",
		fake.requests.lock().unwrap()
	);
}
