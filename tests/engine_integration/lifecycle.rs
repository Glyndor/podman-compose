//! Engine integration tests (split for the source line limit).
use super::*;

// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn up_and_down() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("udn");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let running = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let removed = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();

	assert_eq!(
		running,
		vec![format!("{proj}-web-1")],
		"up returned success without creating the service container"
	);
	assert!(
		removed.is_empty(),
		"down returned success and left containers behind: {removed:?}"
	);
}

#[tokio::test]
async fn up_no_recreate_skips_running() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("nor");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Mark the container's filesystem. A recreate builds a fresh one under the
	// same name, so the marker is what separates "skipped the running container"
	// from "replaced it with an identical-looking one".
	engine
		.test_exec_capture(
			&cname,
			vec![
				"sh".into(),
				"-c".into(),
				"echo first-container > /marker".into(),
			],
		)
		.await
		.unwrap();

	// Second up with no_recreate: already running → skip
	engine
		.up_with_options(&file, false, &[], &[], true, false, false)
		.await
		.unwrap();
	let survived = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/marker".into()])
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();

	assert_eq!(
		survived.trim(),
		"first-container",
		"no_recreate replaced the running container instead of skipping it"
	);
}

#[tokio::test]
async fn up_target_services_only() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("tgt");
	let engine = Engine::new(client, proj.clone());
	// `cache` exists so the targeting has something to leave out. With only db and
	// web declared, starting "web and its dependency" and starting the whole file
	// produce the same containers, so no assertion could tell the two apart.
	let file = parse_str(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      - db\n  cache:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	// Only start web (and its dep db)
	engine
		.up_with_options(&file, false, &[], &["web".to_string()], false, false, false)
		.await
		.unwrap();
	let mut names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	names.sort();
	engine.down(&file).await.unwrap();

	assert_eq!(
		names,
		vec![format!("{proj}-db-1"), format!("{proj}-web-1")],
		"targeting web must start web and its dependency db, and leave cache alone"
	);
}

#[tokio::test]
async fn down_with_remove_volumes() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("dvol");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(&format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - {proj}-data:/data\nvolumes:\n  {proj}-data:\n"
	))
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.down_with_options(&file, true).await.unwrap();
}

#[tokio::test]
async fn restart_all_services() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rsa");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	let started = poll_container_file(&engine, &cname, "/starts", "start", 30).await;

	engine.restart(&file, None).await.unwrap();
	let restarted = poll_container_file(&engine, &cname, "/starts", "start\nstart", 60).await;

	engine.down(&file).await.unwrap();
	assert!(started, "the container never wrote its first start line");
	assert!(
		restarted,
		"restart returned success but the process never started a second time"
	);
}

#[tokio::test]
async fn restart_specific_service() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rss");
	let engine = Engine::new(client, proj.clone());
	// Two services, so naming one is a choice the assertion can check. Restarting
	// everything would satisfy a test that only looked at web.
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n  idle:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let web = format!("{proj}-web-1");
	let idle = format!("{proj}-idle-1");
	let started = poll_container_file(&engine, &web, "/starts", "start", 30).await;

	engine.restart(&file, Some("web")).await.unwrap();
	let restarted = poll_container_file(&engine, &web, "/starts", "start\nstart", 60).await;
	let untouched = engine
		.test_exec_capture(&idle, vec!["cat".into(), "/starts".into()])
		.await
		.unwrap_or_default();

	engine.down(&file).await.unwrap();
	assert!(started, "web never wrote its first start line");
	assert!(
		restarted,
		"restart returned success but web never started a second time"
	);
	assert_eq!(
		untouched.trim(),
		"start",
		"restarting web also restarted idle, which was not named"
	);
}

#[tokio::test]
async fn restart_cascade_dep() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rcd");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n    depends_on:\n      db:\n        condition: service_started\n        restart: true\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let db = format!("{proj}-db-1");
	let web = format!("{proj}-web-1");
	let started = poll_container_file(&engine, &web, "/starts", "start", 30).await;

	// Restarting db triggers cascade restart of web
	engine.restart(&file, Some("db")).await.unwrap();
	let db_restarted = poll_container_file(&engine, &db, "/starts", "start\nstart", 60).await;
	let web_cascaded = poll_container_file(&engine, &web, "/starts", "start\nstart", 60).await;

	engine.down(&file).await.unwrap();
	assert!(started, "web never wrote its first start line");
	assert!(db_restarted, "the named service db never restarted");
	assert!(
		web_cascaded,
		"db restarted but the restart: true dependant web did not follow"
	);
}

#[tokio::test]
async fn restart_unknown_service_fails() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("ruf");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let err = engine
		.restart(&file, Some("nonexistent"))
		.await
		.unwrap_err();
	assert!(matches!(err, podup::ComposeError::ServiceNotFound(_)));
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

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
		.pull_services_with_options(
			&file,
			&[],
			podup::PullOptions {
				ignore_failures: true,
				include_deps: false,
			},
		)
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
	engine.attach_logs(&file).await.unwrap();
	engine.down(&file).await.unwrap();
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
		.up_with_options(&file, false, &[], &[], false, true, false)
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
