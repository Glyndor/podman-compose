use super::volume_owned_by;

#[cfg(unix)]
use crate::engine::fake_podman;
#[cfg(unix)]
use crate::engine::Engine;
#[cfg(unix)]
use crate::error::ComposeError;

#[cfg(unix)]
fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

#[test]
fn volume_owned_by_matches_project_label() {
	let vol = serde_json::json!({
		"Name": "proj_data",
		"Labels": { "podup.project": "proj", "extra": "1" },
	});
	assert!(volume_owned_by(&vol, "proj"));
	// A different project's volume, or one podup never labelled, is not ours.
	assert!(!volume_owned_by(&vol, "other"));
	let unlabelled = serde_json::json!({ "Name": "loose", "Labels": {} });
	assert!(!volume_owned_by(&unlabelled, "proj"));
	let no_labels = serde_json::json!({ "Name": "loose" });
	assert!(!volume_owned_by(&no_labels, "proj"));
}

/// #598 (unfixed on this path until now): `down -p PROJECT` with no compose
/// file only ever warned on a removal failure and unconditionally returned
/// `Ok`. Two labelled containers, one whose force-remove genuinely fails:
/// `down_by_label` must still attempt (and complete) the other before
/// exiting non-zero for the first.
#[tokio::test]
#[cfg(unix)]
async fn down_by_label_propagates_a_real_removal_failure_after_completing_the_rest() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-db-1"],"Labels":{"podup.service":"db"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if method == "POST" && target.contains("/stop") {
			(200, String::new())
		} else if method == "DELETE" && target.contains("/proj-web-1?force=true") {
			(500, r#"{"message":"device or resource busy"}"#.to_string())
		} else if method == "DELETE" && target.contains("/proj-db-1?force=true") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let err = e
		.down_by_label(false)
		.await
		.expect_err("a real container-removal failure must propagate");
	assert!(
		matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
		"got {err:?}"
	);

	// Best-effort: the healthy container must still have been reached even
	// though the other one failed.
	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("/proj-db-1?force=true")),
		"expected proj-db-1 to be removed despite proj-web-1 failing: {seen:?}"
	);
}

/// A `down -p PROJECT` on an already torn-down project (no live containers,
/// nothing left to sweep by label) must still exit 0; idempotency is
/// preserved on the label-only path exactly as on `Engine::down`.
#[tokio::test]
#[cfg(unix)]
async fn down_by_label_on_an_already_torn_down_project_is_still_ok() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	e.down_by_label(false)
		.await
		.expect("a re-run down_by_label on a torn-down project must still exit 0");
}
