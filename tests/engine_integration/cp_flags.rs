//! `cp` flag-parity integration tests (split for the source line limit).
use super::*;

#[tokio::test]
async fn engine_cp_index_out_of_range_errors() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let local = dir.path().join("f.txt");
	fs::write(&local, b"x").unwrap();

	let proj = proj("cpidx");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str("services:\n  web:\n    image: alpine:latest\n").unwrap();

	// Target replica 9 of a single-replica service: must error rather than
	// silently fall back to the first container. The replica-resolution fix
	// reports this as a typed `ReplicaIndex` (named service, 1-based index)
	// instead of a generic `ServiceNotFound`, so the user sees exactly which
	// index is out of range.
	let result = engine
		.cp_with_options(
			&file,
			local.to_str().unwrap(),
			"web:/tmp",
			podup::CpOptions::new(Some(9), false, false),
		)
		.await;
	assert!(
		matches!(
			result,
			Err(podup::ComposeError::ReplicaIndex { ref service, index: 9 }) if service == "web"
		),
		"out-of-range --index must error with ReplicaIndex, got {result:?}"
	);
}

#[cfg(all(unix, feature = "test-helpers"))]
#[tokio::test]
async fn engine_cp_follow_link_uploads_target_contents() {
	use std::os::unix::fs::symlink;
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("target.txt");
	fs::write(&target, b"linked-content").unwrap();
	let link = dir.path().join("link.txt");
	symlink(&target, &link).unwrap();

	let proj = proj("cplnk");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	engine.up(&file).await.unwrap();

	// -L follows the host symlink, so the container receives the target's bytes
	// (a regular file), not a dangling link.
	let result = engine
		.cp_with_options(
			&file,
			link.to_str().unwrap(),
			"web:/tmp",
			podup::CpOptions::new(None, true, false),
		)
		.await;
	let out = engine
		.test_exec_capture(
			&format!("{proj}-web-1"),
			vec!["cat".into(), "/tmp/link.txt".into()],
		)
		.await;
	engine.down(&file).await.unwrap();
	result.unwrap();
	assert!(
		out.unwrap_or_default().contains("linked-content"),
		"-L must upload the symlink target's contents"
	);
}

#[cfg(all(unix, feature = "test-helpers"))]
#[tokio::test]
async fn engine_cp_to_container_renames_a_single_file() {
	// `cp host-file svc:/tmp/newname.txt` must create a FILE named newname.txt,
	// not a directory holding the source, matching `docker cp` rename-on-copy.
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let src = dir.path().join("src.txt");
	fs::write(&src, b"renamed-content").unwrap();

	let proj = proj("cpren");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	engine.up(&file).await.unwrap();

	let result = engine
		.cp(&file, src.to_str().unwrap(), "web:/tmp/renamed.txt")
		.await;
	// `test -f` succeeds only for a regular file; a directory (the old bug) fails it.
	let out = engine
		.test_exec_capture(
			&format!("{proj}-web-1"),
			vec![
				"sh".into(),
				"-c".into(),
				"test -f /tmp/renamed.txt && cat /tmp/renamed.txt".into(),
			],
		)
		.await;
	engine.down(&file).await.unwrap();
	result.unwrap();
	assert!(
		out.unwrap_or_default().contains("renamed-content"),
		"cp to a new name must create a file with the source's content, not a directory"
	);
}
