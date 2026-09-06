//! `exec` replica-resolution integration tests: a bare `exec` and `exec
//! --index N` must reach a *running* replica of a service scaled by an earlier
//! `up --scale`/`scale`, not the compose static count (issue #795). Skip
//! gracefully when Podman is unreachable.
use super::*;

#[cfg(feature = "test-helpers")]
use std::collections::HashMap;

/// Bring up `web` with three replicas, then drive `exec` from a *fresh* engine
/// whose compose file still declares the default single replica: exactly the
/// real CLI case where a later `podup exec` knows nothing of the prior
/// `up --scale`. Before the fix, `exec` resolved the replica count from that
/// static default (1): a bare `exec` targeted the unsuffixed `web` (which does
/// not exist once scaled) and `--index 2` was rejected as "no replica 2".
#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn engine_exec_reaches_live_scaled_replicas() {
	let (scaler_client, exec_client) = match (podman().await, podman().await) {
		(Some(a), Some(b)) => (a, b),
		_ => return,
	};
	let proj = proj("execscale");
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	// Scale to three replicas with a dedicated engine (mirrors `up --scale web=3`).
	let scaler = Engine::new(scaler_client, proj.clone())
		.with_scale_overrides(HashMap::from([("web".to_string(), 3)]));
	scaler.up(&file).await.unwrap();

	// A fresh engine with NO scale override: it sees only the compose default of
	// one replica, just like a separate `podup exec` invocation would.
	let engine = Engine::new(exec_client, proj.clone());

	// `--index 2` must reach the *running* web-2. Drop a marker in it via exec,
	// then read each replica back to prove exactly web-2 was hit.
	let by_index = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".into(),
				"-c".into(),
				"echo idx2 > /tmp/exec_marker".into(),
			],
			podup::ExecOptions::default().with_index(Some(2)),
		)
		.await;

	// A bare `exec` (no index) must reach the lowest-numbered running replica,
	// web-1, not the never-created unsuffixed `web`.
	let bare = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".into(),
				"-c".into(),
				"echo bare > /tmp/exec_bare".into(),
			],
			podup::ExecOptions::default(),
		)
		.await;

	// `--index 4` is out of range against the three running replicas.
	let out_of_range = engine
		.exec_with_options(
			&file,
			"web",
			vec!["true".into()],
			podup::ExecOptions::default().with_index(Some(4)),
		)
		.await;

	let marker_in_2 = engine
		.test_exec_capture(
			&format!("{proj}-web-2"),
			vec!["cat".into(), "/tmp/exec_marker".into()],
		)
		.await;
	let marker_in_1 = engine
		.test_exec_capture(
			&format!("{proj}-web-1"),
			vec![
				"sh".into(),
				"-c".into(),
				"cat /tmp/exec_marker 2>/dev/null || true".into(),
			],
		)
		.await;
	let bare_in_1 = engine
		.test_exec_capture(
			&format!("{proj}-web-1"),
			vec!["cat".into(), "/tmp/exec_bare".into()],
		)
		.await;

	scaler.down(&file).await.unwrap();

	by_index.unwrap();
	bare.unwrap();
	assert!(
		matches!(
			out_of_range,
			Err(podup::ComposeError::ReplicaIndex { ref service, index: 4 }) if service == "web"
		),
		"out-of-range --index against running scale must error, got {out_of_range:?}"
	);
	assert!(
		marker_in_2.unwrap_or_default().contains("idx2"),
		"`exec --index 2` must run inside the running web-2"
	);
	assert!(
		!marker_in_1.unwrap_or_default().contains("idx2"),
		"`exec --index 2` must NOT touch web-1"
	);
	assert!(
		bare_in_1.unwrap_or_default().contains("bare"),
		"bare `exec` must run inside the lowest-numbered running replica web-1"
	);
}
