//! Watch integration tests (require the test-helpers feature).
use std::time::Duration;

use super::*;

#[tokio::test]
async fn watch_no_develop_rules_errors() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("wno");
	let engine = Engine::new(client, proj.clone());
	// No develop.watch section → watch() errors, matching docker compose, which
	// reports "none of the selected services is configured for watch" rather than
	// silently exiting 0 (an invocation with nothing to watch is almost always a
	// misconfiguration the user wants flagged).
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let err = engine
		.watch(&file)
		.await
		.expect_err("watch with no rules must error");
	assert!(
		matches!(err, podup::ComposeError::Watch(_)),
		"expected a Watch error, got {err:?}"
	);
}

#[tokio::test]
async fn watch_sync_file_to_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let src_file = dir.path().join("app.txt");
	fs::write(&src_file, b"initial content").unwrap();

	let proj = proj("wsy");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	engine
		.test_sync_to_container(&cname, &src_file, "/tmp/app.txt")
		.await
		.unwrap();
	let first = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/tmp/app.txt".into()])
		.await
		.unwrap_or_default();

	// Sync the same path again with different bytes. A watch rule fires on every
	// change to the path it covers, so a second copy over an existing file is the
	// ordinary case rather than an edge one, and it is the half a first-copy-only
	// assertion cannot see.
	fs::write(&src_file, b"changed content").unwrap();
	// The result is captured instead of unwrapped, deliberately. On Podman 6 this
	// second copy came back as the #1097 could-not-confirm error, which says in
	// its own text that the copy may or may not have landed. #1270 owns that
	// error-reporting defect. What this test owns is whether the bytes arrived,
	// so it reads them and reports what the sync claimed alongside — which is
	// also the measurement #1270 needs to tell its branches apart.
	let reported = engine
		.test_sync_to_container(&cname, &src_file, "/tmp/app.txt")
		.await;
	let second = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/tmp/app.txt".into()])
		.await
		.unwrap_or_default();

	engine.down(&file).await.unwrap();
	assert_eq!(
		first.trim(),
		"initial content",
		"sync returned success but the file never reached the container"
	);
	assert_eq!(
		second.trim(),
		"changed content",
		"the second sync left the first copy in place; it reported {reported:?}"
	);
}

#[tokio::test]
async fn watch_restart_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("wrs");
	let engine = Engine::new(client, proj.clone());
	// One line appended per start, on the container's own filesystem, which
	// survives a restart. That makes the line count a container-scoped record of
	// how many times the process started. See the note in
	// watch_sync_and_restart_does_both for why /proc/uptime is not an option.
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Pin the fixture before acting. A counter that had not written its first
	// line yet, or that wrote more than one, would make the check below fail for
	// a reason that has nothing to do with restart.
	let started = poll_synced(&engine, &cname, "/starts", "start", 30).await;
	let before = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/starts".into()])
		.await
		.unwrap_or_default();

	engine.test_watch_restart(&cname).await.unwrap();
	let restarted = poll_synced(&engine, &cname, "/starts", "start\nstart", 60).await;

	engine.down(&file).await.unwrap();
	assert!(started, "the container never wrote its first start line");
	assert_eq!(
		before.trim(),
		"start",
		"the fixture wrote more than one start line before the restart"
	);
	assert!(
		restarted,
		"restart returned success but the container process never started again"
	);
}

#[tokio::test]
async fn watch_exec_in_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("wex");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Write a marker to the container's filesystem instead of echoing to a stream
	// nobody reads: the action drains the exec's output and discards it, so an
	// echo leaves nothing an assertion can reach.
	engine
		.test_watch_exec(
			&cname,
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"echo from-watch-exec >> /exec-ran".to_string(),
			],
		)
		.await
		.unwrap();
	let out = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/exec-ran".into()])
		.await
		.unwrap_or_default();

	engine.down(&file).await.unwrap();
	// Exactly one line, so the action ran the command once and ran it in the
	// container rather than on the host.
	assert_eq!(
		out.trim(),
		"from-watch-exec",
		"the watch exec action did not run the command inside the container"
	);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_initial_sync_runs() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let src = dir.path().join("app.txt");
	fs::write(&src, b"initial").unwrap();

	let proj = proj("wis");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    develop:\n      watch:\n        - path: app.txt\n          action: sync\n          target: /tmp/app.txt\n          initial_sync: true\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();

	let client2 = podup::podman::connect_from_env()
		.or_else(|_| podup::podman::connect(None))
		.unwrap();
	let engine2 = Engine::with_base_dir(client2, proj.clone(), dir.path().to_path_buf());
	let file2 = file.clone();
	let handle = tokio::spawn(async move { engine2.watch(&file2).await });

	// Poll for the observable effect of initial_sync (the file appearing in the
	// container) instead of sleeping a fixed duration and assuming it ran.
	let cname = format!("{proj}-web-1");
	let synced = poll_synced(&engine, &cname, "/tmp/app.txt", "initial", 60).await;

	handle.abort();
	engine.down(&file).await.unwrap();
	assert!(
		synced,
		"initial_sync did not copy the file into the container"
	);
}

#[tokio::test]
async fn watch_sync_creates_missing_target_directory() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let src_file = dir.path().join("app.txt");
	fs::write(&src_file, b"created").unwrap();

	let proj = proj("wcd");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Sync into a directory that does not exist in the image; podup must create
	// it (like docker compose watch) rather than fail.
	engine
		.test_sync_to_container(&cname, &src_file, "/newdir/app.txt")
		.await
		.unwrap();
	let out = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/newdir/app.txt".into()])
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	assert!(
		out.contains("created"),
		"sync did not create the missing target directory: {out:?}"
	);
}

/// `rebuild` was the one watch action with no coverage at all, and it is the
/// only one that goes all the way back through `build` and container recreation
/// rather than touching a running container. It reads its trigger file into the
/// image, so a rebuild that silently did nothing — or that rebuilt the image and
/// left the old container running — is visible as stale content.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_rebuild_recreates_the_container_from_the_new_image() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(dir.path().join("app.txt"), b"v1").unwrap();
	fs::write(
		dir.path().join("Dockerfile"),
		b"FROM alpine:latest\nCOPY app.txt /app.txt\nCMD [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let proj = proj("wrb");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    build: .\n    develop:\n      watch:\n        - path: app.txt\n          action: rebuild\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// The image really did carry v1 before the change, so a stale read later means
	// the rebuild did not happen rather than the fixture never being right.
	let before = poll_synced(&engine, &cname, "/app.txt", "v1", 30).await;

	let client2 = podup::podman::connect_from_env()
		.or_else(|_| podup::podman::connect(None))
		.unwrap();
	let engine2 = Engine::with_base_dir(client2, proj.clone(), dir.path().to_path_buf());
	let file2 = file.clone();
	let handle = tokio::spawn(async move { engine2.watch(&file2).await });

	// Give the watcher a moment to register before changing the file, then poll
	// for the effect rather than assuming a fixed rebuild duration.
	tokio::time::sleep(Duration::from_secs(2)).await;
	fs::write(dir.path().join("app.txt"), b"v2").unwrap();
	let rebuilt = poll_synced(&engine, &cname, "/app.txt", "v2", 120).await;

	handle.abort();
	let containers = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();

	assert!(before, "the image did not carry v1 before the change");
	assert!(
		rebuilt,
		"the container still serves the old image, so rebuild did not recreate it"
	);
	assert_eq!(
		containers.len(),
		1,
		"rebuild left the previous container behind: {containers:?}"
	);
}

/// `sync+restart` is two effects in one action, and a test that only checks the
/// file arrived would pass with the restart half missing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn watch_sync_and_restart_does_both() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(dir.path().join("app.txt"), b"synced-value").unwrap();

	let proj = proj("wsr");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	// The command appends one line per start. A restart re-runs it against the
	// same (persisting) filesystem, so the line count is a container-scoped
	// counter of how many times the process was started — unlike /proc/uptime,
	// which is not namespaced and would report the host's.
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n    develop:\n      watch:\n        - path: app.txt\n          action: sync+restart\n          target: /tmp/app.txt\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");

	let client2 = podup::podman::connect_from_env()
		.or_else(|_| podup::podman::connect(None))
		.unwrap();
	let engine2 = Engine::with_base_dir(client2, proj.clone(), dir.path().to_path_buf());
	let file2 = file.clone();
	let handle = tokio::spawn(async move { engine2.watch(&file2).await });

	tokio::time::sleep(Duration::from_secs(2)).await;
	fs::write(dir.path().join("app.txt"), b"changed-value").unwrap();
	let synced = poll_synced(&engine, &cname, "/tmp/app.txt", "changed-value", 60).await;

	// Poll for the second start line rather than sleeping and hoping.
	let restarted = poll_synced(&engine, &cname, "/starts", "start\nstart", 60).await;

	handle.abort();
	engine.down(&file).await.unwrap();
	assert!(synced, "sync+restart did not copy the file");
	assert!(
		restarted,
		"sync+restart copied the file but never restarted the container"
	);
}

/// Poll until `cat`-ing `path` in the container yields `expect`, or `secs`
/// elapse. Read-only: used when the trigger already happened (initial_sync).
async fn poll_synced(engine: &Engine, cname: &str, path: &str, expect: &str, secs: u64) -> bool {
	let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
	while tokio::time::Instant::now() < deadline {
		if let Ok(out) = engine
			.test_exec_capture(cname, vec!["cat".into(), path.into()])
			.await
		{
			if out.contains(expect) {
				return true;
			}
		}
		tokio::time::sleep(Duration::from_millis(100)).await;
	}
	false
}
