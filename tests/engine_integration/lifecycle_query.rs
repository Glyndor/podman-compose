//! Engine integration tests for the query and inspection commands.
//!
//! Split out of `lifecycle.rs` when that file passed the 500 code-line hard
//! limit; the two halves are the lifecycle commands and the ones that read back
//! what those commands did.
use super::*;

// Query
// ---------------------------------------------------------------------------

/// `Engine::ps` returns `Result<()>` and writes the table to stdout, so at this
/// level the only thing to assert is that listing a running project does not
/// error. What it prints is checked where it is reachable, in
/// `cli_lifecycle::cli_ps_subcommand`, which reads the container name and the
/// STATUS column — the column #590 left empty for every container.
#[tokio::test]
async fn ps_shows_running_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("ps");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.ps(&file).await.unwrap();
	engine.down(&file).await.unwrap();
}

/// Same shape as `ps`: `Engine::logs` streams to stdout and returns
/// `Result<()>`. The output contract — the container's own line, carrying the
/// `service |` prefix that #594 dropped — is asserted in
/// `cli_lifecycle::cli_logs_subcommand`.
#[tokio::test]
async fn logs_from_named_service() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("lgs");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo hello && sleep infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.logs(&file, Some("web"), false).await.unwrap();
	engine.down(&file).await.unwrap();
}

/// The all-services variant of the above, and unassertable here for the same
/// reason. Kept because it exercises the no-target branch of the same call.
#[tokio::test]
async fn logs_all_services() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("lga");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.logs(&file, None, false).await.unwrap();
	engine.down(&file).await.unwrap();
}

#[tokio::test]
async fn logs_unknown_service_fails() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("lgf");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let err = engine
		.logs(&file, Some("nonexistent"), false)
		.await
		.unwrap_err();
	assert!(matches!(err, podup::ComposeError::ServiceNotFound(_)));
}

#[tokio::test]
async fn exec_command_in_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("exc");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Leave the result on the container's filesystem. `echo` alone writes to a
	// stream the test cannot reach, so an exec that ran nowhere looked the same
	// as one that ran.
	engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"echo exec-ran > /exec-marker".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await
		.unwrap();
	let marker = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/exec-marker".into()])
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();

	assert_eq!(
		marker.trim(),
		"exec-ran",
		"exec returned success without running the command in the container"
	);
}

#[tokio::test]
async fn exec_with_options_user_workdir_env() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("excopt");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Write the three values out instead of printing them. A bad workdir or user
	// does make the exec error, so the old test caught those, but it could not
	// see an option that was accepted and then dropped on the floor: podup could
	// have sent no workdir at all and the exec would still have succeeded, in /.
	engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"{ pwd; echo $FOO; id -un; } > /opts".to_string(),
			],
			podup::ExecOptions::default()
				.with_user(Some("root".to_string()))
				.with_workdir(Some("/tmp".to_string()))
				.with_env(vec!["FOO=bar".to_string()]),
		)
		.await
		.unwrap();
	let opts = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/opts".into()])
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();

	assert_eq!(
		opts.trim(),
		"/tmp\nbar\nroot",
		"exec did not apply workdir, env and user to the command"
	);
}

#[tokio::test]
async fn exec_unknown_service_fails() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("exf");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let err = engine
		.exec_with_options(
			&file,
			"nonexistent",
			vec!["echo".to_string()],
			podup::ExecOptions::default(),
		)
		.await
		.unwrap_err();
	assert!(matches!(err, podup::ComposeError::ServiceNotFound(_)));
}

#[tokio::test]
async fn exec_nonexistent_user_fails_fast() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("exbu");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// A nonexistent named user must surface a prompt, clear error — never hang for
	// the full client read timeout (~120s) and then report a misleading
	// socket-timeout (issue #720).
	let started = std::time::Instant::now();
	let err = engine
		.exec_with_options(
			&file,
			"web",
			vec!["echo".to_string(), "hi".to_string()],
			podup::ExecOptions::default().with_user(Some("definitelynosuchuser".to_string())),
		)
		.await
		.unwrap_err();
	let elapsed = started.elapsed();
	engine.down(&file).await.unwrap();

	assert!(
		elapsed < std::time::Duration::from_secs(60),
		"exec with a bad user must fail fast, took {elapsed:?}"
	);
	let msg = err.to_string().to_ascii_lowercase();
	// Either the engine's prompt diagnostic (it names the user / passwd file) or
	// podup's exec-specific timeout message — but never the bare socket-timeout.
	assert!(
		msg.contains("user") || msg.contains("passwd") || msg.contains("exec"),
		"unexpected error for a bad exec user: {msg}"
	);
	assert!(
		!msg.contains("waiting for the podman socket"),
		"bad-user exec leaked a socket-timeout message: {msg}"
	);
	// A normal exec into the same service still works after the failure.
	engine.up(&file).await.unwrap();
	engine
		.exec_with_options(
			&file,
			"web",
			vec!["echo".to_string(), "ok".to_string()],
			podup::ExecOptions::default(),
		)
		.await
		.unwrap();
	engine.down(&file).await.unwrap();
}

/// `Engine::pull` reports progress on stderr and returns `Result<()>`. Asserting
/// that the image is present afterwards would prove nothing: `alpine:latest` is
/// already local in every environment this suite runs in, so the assertion would
/// hold with the pull removed entirely. Removing it first would make the test
/// depend on the network, which the testing standard rules out. What `pull`
/// reports is asserted in `cli_lifecycle::cli_pull_subcommand`.
#[tokio::test]
async fn pull_images() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("pll");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.pull(&file).await.unwrap();
}

#[tokio::test]
async fn pull_ignore_failures_continues_past_bad_image() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("plif");
	let engine = Engine::new(client, proj.clone());
	// A bogus registry/image alongside a good one: the bad pull fails.
	let file = parse_str(
		"services:\n  good:\n    image: alpine:latest\n  bad:\n    image: localhost:1/nope:nope\n",
	)
	.unwrap();

	// Without --ignore-pull-failures the bad image aborts the whole pull.
	let strict = engine.pull(&file).await;
	assert!(strict.is_err(), "bad image must fail a strict pull");

	// With --ignore-pull-failures the failure is logged and pull returns Ok.
	let lenient = engine
		.pull_services_with_options(&file, &[], podup::PullOptions::new(true, false))
		.await;
	assert!(
		lenient.is_ok(),
		"ignore-pull-failures must not abort: {lenient:?}"
	);
}

#[tokio::test]
async fn remove_orphans_no_orphans() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("orp");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.remove_orphans(&file).await.unwrap();
	// The point of this test is what remove_orphans must NOT do. With no orphan
	// present, a sweep that removed the project's own container would still have
	// returned success.
	let survivors = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();

	assert_eq!(
		survivors,
		vec![format!("{proj}-web-1")],
		"remove_orphans removed a container that belongs to the project"
	);
}

#[tokio::test]
async fn attach_logs_empty_attach_returns() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("atl");
	let engine = Engine::new(client, proj.clone());
	// attach: false — attach_logs finds no targets and returns immediately
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    attach: false\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// "Returns immediately" is the claim, and it is the one thing here that can be
	// checked without reading the stream: attaching to a service marked
	// `attach: false` would follow the container's output and never come back, so
	// the test would hang rather than fail. A bound turns that into a red.
	let started = tokio::time::Instant::now();
	let attached = tokio::time::timeout(
		std::time::Duration::from_secs(10),
		engine.attach_logs(&file),
	)
	.await;
	let elapsed = started.elapsed();
	engine.down(&file).await.unwrap();

	let attached =
		attached.expect("attach_logs did not return within 10s on an attach: false service");
	attached.unwrap();
	assert!(
		elapsed < std::time::Duration::from_secs(5),
		"attach_logs took {elapsed:?} on a service with attach: false; it has no targets and must not wait"
	);
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn up_skips_recreate_when_config_unchanged() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rch");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// A recreate builds a fresh filesystem under the same container name, so a
	// marker written into the old one is what tells the three outcomes apart.
	// Every phase below returned success before this test asserted anything, so
	// nothing here distinguished "skipped" from "recreated".
	let write_marker = vec![
		"sh".to_string(),
		"-c".to_string(),
		"echo marked > /marker".to_string(),
	];
	let read_marker = vec!["cat".to_string(), "/marker".to_string()];
	engine
		.test_exec_capture(&cname, write_marker.clone())
		.await
		.unwrap();

	// Same config again -> config-hash matches -> skip recreate + ensure started.
	engine.up(&file).await.unwrap();
	let after_same = engine
		.test_exec_capture(&cname, read_marker.clone())
		.await
		.unwrap_or_default();

	// force_recreate -> recreate even though config is unchanged.
	engine
		.up_with_options(&file, false, &[], &[], false, true, false, false)
		.await
		.unwrap();
	let after_force = engine
		.test_exec_capture(&cname, read_marker.clone())
		.await
		.unwrap_or_default();

	// Changed config -> hash differs -> recreate.
	engine
		.test_exec_capture(&cname, write_marker)
		.await
		.unwrap();
	let file2 = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"120\"]\n",
	)
	.unwrap();
	engine.up(&file2).await.unwrap();
	let after_change = engine
		.test_exec_capture(&cname, read_marker)
		.await
		.unwrap_or_default();

	engine.down(&file2).await.unwrap();
	assert_eq!(
		after_same.trim(),
		"marked",
		"an unchanged config recreated the container instead of skipping it"
	);
	assert_ne!(
		after_force.trim(),
		"marked",
		"force_recreate skipped the container instead of recreating it"
	);
	assert_ne!(
		after_change.trim(),
		"marked",
		"a changed config skipped the container instead of recreating it"
	);
}
