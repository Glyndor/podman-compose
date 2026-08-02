//! Engine integration tests (split for the source line limit).
use super::*;

// ---------------------------------------------------------------------------
// Pause / unpause
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pause_and_unpause() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("pau");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// A paused container refuses new exec sessions: Podman answers "can only
	// create exec sessions on running containers". That refusal is the observable
	// difference between a pause that happened and one that returned Ok having
	// done nothing, which is all this test used to check.
	let before = engine.test_exec_capture(&cname, vec!["true".into()]).await;
	engine.pause(&file, &[]).await.unwrap();
	let while_paused = engine.test_exec_capture(&cname, vec!["true".into()]).await;
	engine.unpause(&file, &[]).await.unwrap();
	let after = engine.test_exec_capture(&cname, vec!["true".into()]).await;
	engine.down(&file).await.unwrap();

	assert!(
		before.is_ok(),
		"the container did not accept an exec before being paused: {before:?}"
	);
	assert!(
		while_paused.is_err(),
		"pause returned success but the container still accepted an exec"
	);
	assert!(
		after.is_ok(),
		"unpause did not return the container to running: {after:?}"
	);
}

// ---------------------------------------------------------------------------
// Run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_run_command_succeeds() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("run");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str("services:\n  job:\n    image: alpine:latest\n").unwrap();

	let result = engine
		.run(
			&file,
			"job",
			podup::RunOptions {
				cmd: vec!["echo".to_string(), "hello".to_string()],
				rm: true,
				..Default::default()
			},
		)
		.await;
	assert!(result.is_ok(), "run failed: {result:?}");
}

#[tokio::test]
async fn engine_run_nonzero_exit_returns_run_exited() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rxc");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str("services:\n  job:\n    image: alpine:latest\n").unwrap();

	let result = engine
		.run(
			&file,
			"job",
			podup::RunOptions {
				cmd: vec!["false".to_string()],
				rm: true,
				..Default::default()
			},
		)
		.await;
	assert!(
		matches!(result, Err(podup::ComposeError::RunExited(_))),
		"expected RunExited, got {result:?}"
	);
}

// ---------------------------------------------------------------------------
// Top
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_top_running_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("top");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.top(&file, &[]).await.unwrap();
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
// Images
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_images_lists_service_images() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("img");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.images(&file).await.unwrap();
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
// Port
// ---------------------------------------------------------------------------

/// `Engine::port` returns `Result<()>` and writes the binding to stdout, so this
/// can only assert that a published port resolves without error. The binding
/// itself is checked where it is observable, in
/// `stats_flags::cli_port_prints_the_published_binding` — this test used to be
/// named for a return value the API does not have.
#[tokio::test]
async fn engine_port_resolves_a_published_port() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("prt");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    ports:\n      - \"127.0.0.1:18080:80\"\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.port(&file, "web", "80", "tcp").await.unwrap();
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
// Cp
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_cp_from_container_extracts_file() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("cpf");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();

	let dst = dir.path().to_str().unwrap().to_string();
	let src = "web:/etc/hostname".to_string();
	let result = engine.cp(&file, &src, &dst).await;
	engine.down(&file).await.unwrap();

	result.unwrap();
	assert!(dir.path().join("hostname").exists());
}

#[tokio::test]
async fn engine_cp_to_container_uploads_file() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let local_file = dir.path().join("testfile.txt");
	fs::write(&local_file, b"hello from host").unwrap();

	let proj = proj("cpt");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();

	let src = local_file.to_str().unwrap().to_string();
	let dst = "web:/tmp".to_string();
	let result = engine.cp(&file, &src, &dst).await;
	// `cp` reporting success is not the same as the file arriving — that exact
	// false success is what #1097 was on Podman 6, where libpod accepted the
	// archive, closed the connection without a response, and podup called it done.
	// Read the copy back out of the container before tearing anything down.
	let landed = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"test \"$(cat /tmp/testfile.txt)\" = 'hello from host'".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await;
	engine.down(&file).await.unwrap();

	result.unwrap();
	assert!(
		landed.is_ok(),
		"cp reported success but /tmp/testfile.txt is missing or has the wrong contents: {landed:?}"
	);
}

// ---------------------------------------------------------------------------
// Replicas: restart, logs, top, exec, port target correct containers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn restart_scaled_service_all_replicas() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rsr");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let one = format!("{proj}-worker-1");
	let two = format!("{proj}-worker-2");
	let started = poll_container_file(&engine, &two, "/starts", "start", 30).await;

	// Both replicas must be reachable for restart to succeed. "All replicas" is
	// the claim, and restarting only the first satisfied the old version.
	engine.restart(&file, Some("worker")).await.unwrap();
	let first_restarted = poll_container_file(&engine, &one, "/starts", "start\nstart", 60).await;
	let second_restarted = poll_container_file(&engine, &two, "/starts", "start\nstart", 60).await;

	engine.down(&file).await.unwrap();
	assert!(
		started,
		"the second replica never wrote its first start line"
	);
	assert!(first_restarted, "the first replica did not restart");
	assert!(
		second_restarted,
		"restart stopped at the first replica and left the second running"
	);
}

#[tokio::test]
async fn logs_scaled_service_all_replicas() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("lsr");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo hello && sleep infinity\"]\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// logs for a named service with replicas: should stream from all without error.
	engine.logs(&file, Some("worker"), false).await.unwrap();
	engine.down(&file).await.unwrap();
}

#[tokio::test]
async fn top_scaled_service_all_replicas() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("tsr");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.top(&file, &[]).await.unwrap();
	engine.down(&file).await.unwrap();
}

/// #1250: a project where one service has already run to completion — a
/// `migrate` that exits 0 is the everyday shape — used to abort `top` on the
/// stopped container, losing the services it had not reached yet and exiting
/// non-zero. Measured against `docker compose top` v5.1.3 on the same Podman
/// socket: it omits the stopped service, prints the rest and exits 0.
#[tokio::test]
async fn top_skips_a_stopped_service_and_reports_the_rest() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("tss");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  migrate:\n    image: alpine:latest\n    command: [\"true\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();

	// `migrate` has to have actually exited before `top` runs, or this passes for
	// the wrong reason — `up` returns once the container is started, not once it
	// is done, so without this the test could exercise two running services and
	// prove nothing. `wait` blocks until it stops, which is deterministic where
	// a sleep is not.
	engine
		.wait_services(&file, &["migrate".to_string()])
		.await
		.unwrap();

	engine
		.top(&file, &[])
		.await
		.expect("top must skip the stopped service rather than abort on it");

	engine.down(&file).await.unwrap();
}

#[tokio::test]
async fn exec_scaled_service_targets_first_replica() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("esr");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// Leave a mark instead of echoing, and read it from BOTH replicas. "targets
	// the first replica" has two halves, and an exec that hit replica 2 — or one
	// that somehow hit both — returned Ok just as happily as the right one.
	engine
		.exec_with_options(
			&file,
			"worker",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"echo reached > /which".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await
		.unwrap();
	let first = engine
		.test_exec_capture(
			&format!("{proj}-worker-1"),
			vec!["cat".into(), "/which".into()],
		)
		.await
		.unwrap_or_default();
	let second = engine
		.test_exec_capture(
			&format!("{proj}-worker-2"),
			vec!["cat".into(), "/which".into()],
		)
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();

	assert_eq!(
		first.trim(),
		"reached",
		"exec on a scaled service did not run in the first replica"
	);
	assert_ne!(
		second.trim(),
		"reached",
		"exec on a scaled service ran in the second replica too"
	);
}

#[tokio::test]
async fn port_scaled_service_targets_first_replica() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("psr");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    ports:\n      - \"80\"\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.port(&file, "worker", "80", "tcp").await.unwrap();
	// --index targets a valid replica; an out-of-range index errors.
	engine
		.port_with_index(&file, "worker", "80", "tcp", Some(2))
		.await
		.unwrap();
	let bad = engine
		.port_with_index(&file, "worker", "80", "tcp", Some(9))
		.await;
	assert!(
		matches!(bad, Err(podup::ComposeError::ServiceNotFound(_))),
		"out-of-range port --index must error, got {bad:?}"
	);
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
// Idempotent re-up over an existing named volume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn up_is_idempotent_over_existing_named_volume() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("idv");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - data:/data\nvolumes:\n  data:\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// A second `up` must succeed even though the named volume already exists.
	// Podman's libpod volume-create returns HTTP 500 (not 409) for a duplicate
	// name, so a re-up previously aborted here.
	let second = engine.up(&file).await;
	engine.down(&file).await.unwrap();
	second.expect("second up over an existing named volume must be idempotent");
}

// ---------------------------------------------------------------------------
// A sibling resolves a service by its service name on a shared network
// ---------------------------------------------------------------------------

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn sibling_resolves_service_by_name_on_shared_network() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("dns");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  server:\n    image: busybox:latest\n    command: [\"sh\", \"-c\", \"mkdir -p /www; echo ok > /www/index.html; exec httpd -f -p 80 -h /www\"]\n    networks:\n      - appnet\n  client:\n    image: busybox:latest\n    command: [\"sleep\", \"infinity\"]\n    networks:\n      - appnet\nnetworks:\n  appnet:\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// The client must reach the server by its compose service name (`server`),
	// not only by the container name — the service name has to be registered as
	// a network alias. Retry briefly while the server's httpd comes up.
	let out = engine
		.test_exec_capture(
			&format!("{proj}-client-1"),
			vec![
				"sh".into(),
				"-c".into(),
				"for i in $(seq 1 30); do wget -q -O - http://server:80/ && exit 0; sleep 0.3; done; exit 1".into(),
			],
		)
		.await;
	engine.down(&file).await.unwrap();
	let out = out.expect("exec in client container failed");
	assert!(
		out.contains("ok"),
		"service `server` was not reachable by its service name: {out:?}"
	);
}

// ---------------------------------------------------------------------------
// With NO `networks:` block, services still reach each other by service name
// (the synthesized `default` network — docker-compose parity, #417)
// ---------------------------------------------------------------------------

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn sibling_resolves_service_by_name_without_networks_block() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("dnsdef");
	let engine = Engine::new(client, proj.clone());

	// No top-level `networks:` and no per-service `networks:` — the common case.
	// Parse through the real CLI entry point so the implicit `default` network
	// is synthesized; `parse_str` deliberately does not normalize.
	let dir = tempfile::tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  server:\n    image: busybox:latest\n    command: [\"sh\", \"-c\", \"mkdir -p /www; echo ok > /www/index.html; exec httpd -f -p 80 -h /www\"]\n  client:\n    image: busybox:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let file = parse_files_with_env_files(&[compose], &[]).unwrap();

	engine.up(&file).await.unwrap();
	let out = engine
		.test_exec_capture(
			&format!("{proj}-client-1"),
			vec![
				"sh".into(),
				"-c".into(),
				"for i in $(seq 1 30); do wget -q -O - http://server:80/ && exit 0; sleep 0.3; done; exit 1".into(),
			],
		)
		.await;
	engine.down(&file).await.unwrap();
	let out = out.expect("exec in client container failed");
	assert!(
		out.contains("ok"),
		"service `server` was not reachable by name without a networks: block: {out:?}"
	);
}

// ---------------------------------------------------------------------------
// up -V/--renew-anon-volumes and up --timestamps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn engine_up_renew_anon_volumes_recreates_cleanly() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("renew");
	// `with_renew_anon_volumes` makes the recreate delete drop old anon volumes
	// (v=true); a forced re-up must still succeed.
	let engine = Engine::new(client, proj.clone()).with_renew_anon_volumes(true);
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// Force recreate: the v=true delete path runs for the existing container.
	let second = engine
		.up_with_options(&file, false, &[], &[], false, true, false)
		.await;
	engine.down(&file).await.unwrap();
	second.expect("forced re-up with --renew-anon-volumes must succeed");
}

#[tokio::test]
async fn engine_attach_logs_timestamps_returns_when_container_exits() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("ats");
	let engine = Engine::new(client, proj.clone());
	// A short-lived container: its follow log stream closes on exit, so
	// attach_logs returns instead of blocking.
	let file =
		parse_str("services:\n  job:\n    image: alpine:latest\n    command: [\"echo\", \"hi\"]\n")
			.unwrap();

	engine.up(&file).await.unwrap();
	let res = tokio::time::timeout(
		std::time::Duration::from_secs(20),
		engine.attach_logs_with_options(&file, true),
	)
	.await;
	engine.down(&file).await.unwrap();
	assert!(
		res.is_ok(),
		"attach_logs --timestamps did not return for an exited container"
	);
	res.unwrap().unwrap();
}
