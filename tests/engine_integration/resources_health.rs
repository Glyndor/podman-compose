//! Engine integration tests (split for the source line limit).
use super::*;

// Volumes, secrets, configs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn named_volume_created_on_up() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("nvol");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(&format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - {proj}-data:/data\nvolumes:\n  {proj}-data:\n"
	))
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Write into the mount, then force a recreate. A named volume outlives the
	// container that mounted it, so the file has to still be there afterwards. If
	// /data were ordinary container filesystem, the recreate would take it along,
	// and `up` returning Ok would look exactly the same either way.
	//
	// Known limit, measured rather than assumed: this does NOT detect podup
	// skipping the volume creation. With `create_volumes` stubbed out to a no-op
	// the test still passed, because Podman creates a named volume on first use
	// anyway. What explicit creation adds is the `podup.project` label that makes
	// `down --volumes` recognise the volume as the project's, and reading a
	// volume's labels is not something the library returns. Anyone tightening
	// this needs that, not another assertion on the contents.
	engine
		.test_exec_capture(
			&cname,
			vec![
				"sh".into(),
				"-c".into(),
				"echo in-volume > /data/marker".into(),
			],
		)
		.await
		.unwrap();
	engine
		.up_with_options(&file, false, &[], &[], false, true, false)
		.await
		.unwrap();
	let after = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/data/marker".into()])
		.await
		.unwrap_or_default();
	engine.down_with_options(&file, true).await.unwrap();

	assert_eq!(
		after.trim(),
		"in-volume",
		"the mount did not survive a container recreate, so it was not backed by the named volume"
	);
}

#[tokio::test]
async fn inline_secret_materialized() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("sec");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    secrets:\n      - mysecret\nsecrets:\n  mysecret:\n    content: \"supersecret\"\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// Inline content is created as a Podman-native secret and mounted at the
	// usual /run/secrets/<name> path. `cat` alone only proves the path exists —
	// it exits 0 on an empty file too — so compare the bytes.
	let read = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"test \"$(cat /run/secrets/mysecret)\" = supersecret".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await;
	engine.down(&file).await.unwrap();
	assert!(
		read.is_ok(),
		"the inline secret did not reach /run/secrets/mysecret with its content: {read:?}"
	);
}

#[tokio::test]
async fn file_secret_bound() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let secret_file = dir.path().join("my_secret.txt");
	fs::write(&secret_file, b"file-secret-content").unwrap();

	let proj = proj("fsec");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let yaml = format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    secrets:\n      - filesecret\nsecrets:\n  filesecret:\n    file: {}\n",
		secret_file.display()
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	// The container must READ the secret, not merely be started with it attached.
	// Asserting only that `up` and `down` succeed is what let a `file:` secret stay
	// unreadable on every SELinux-enforcing host for the life of the feature: the
	// mount was there, at the right path, with the right bytes behind it, and the
	// container was denied the open. `run` propagates the command's exit code, so
	// the read is checked without a sleep standing in for synchronisation.
	let read = engine
		.run(
			&file,
			"web",
			podup::RunOptions {
				cmd: vec![
					"sh".to_string(),
					"-c".to_string(),
					"test \"$(cat /run/secrets/filesecret)\" = file-secret-content".to_string(),
				],
				rm: true,
				..Default::default()
			},
		)
		.await;
	engine.down(&file).await.unwrap();
	assert!(
		read.is_ok(),
		"the container could not read /run/secrets/filesecret: {read:?}"
	);
}

#[test]
fn env_secret_materialized() {
	let rt = tokio::runtime::Runtime::new().unwrap();
	temp_env::with_var("PODUP_TEST_SECRET_VAR", Some("env-secret-value"), || {
		rt.block_on(async {
			let client = match podman().await {
				Some(d) => d,
				None => return,
			};
			let proj = proj("esec");
			let engine = Engine::new(client, proj.clone());
			let file = parse_str(
				"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    secrets:\n      - envsecret\nsecrets:\n  envsecret:\n    environment: PODUP_TEST_SECRET_VAR\n",
			)
			.unwrap();

			engine.up(&file).await.unwrap();
			// The point of an `environment:` source is that the variable's value
			// becomes the secret. Starting the container proves neither half.
			let read = engine
				.exec_with_options(
					&file,
					"web",
					vec![
						"sh".to_string(),
						"-c".to_string(),
						"test \"$(cat /run/secrets/envsecret)\" = env-secret-value".to_string(),
					],
					podup::ExecOptions::default(),
				)
				.await;
			engine.down(&file).await.unwrap();
			assert!(
				read.is_ok(),
				"the env-sourced secret did not carry the variable's value: {read:?}"
			);
		});
	});
}

#[tokio::test]
async fn invalid_secret_name_rejected() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("isec");
	let engine = Engine::new(client, proj.clone());
	// A traversal name must be refused: it would otherwise become part of a
	// project-scoped Podman secret name and a URL query parameter.
	//
	// This test used to declare a perfectly ordinary secret called `evils`, discard
	// both results with `let _ =`, and say in a comment that it could not test the
	// thing its name promises. It asserted nothing at all, in either direction.
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    secrets:\n      - '../evil'\nsecrets:\n  '../evil':\n    content: bad\n",
	)
	.unwrap();

	let result = engine.up(&file).await;
	let _ = engine.down(&file).await;
	assert!(
		result.is_err(),
		"a secret named '../evil' was accepted instead of being rejected"
	);
}

#[tokio::test]
async fn inline_config_materialized() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("cfg");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    configs:\n      - mycfg\nconfigs:\n  mycfg:\n    content: \"key=value\"\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// A config defaults to an absolute container-root path, unlike a secret's
	// /run/secrets/<name> — so this also pins the default target, not just the
	// content.
	let read = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"test \"$(cat /mycfg)\" = key=value".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await;
	engine.down(&file).await.unwrap();
	assert!(
		read.is_ok(),
		"the inline config did not reach /mycfg with its content: {read:?}"
	);
}

// ---------------------------------------------------------------------------
// Lifecycle hooks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn post_start_and_pre_stop_hooks_run() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("hks");
	let dir = tempfile::tempdir().unwrap();
	let engine = Engine::new(client, proj.clone());
	// The hooks used to `echo` into a stream nothing reads, so a hook that never
	// ran looked the same as one that did. They append to a bind-mounted host
	// directory instead, which is also the only way to observe `pre_stop`: it
	// fires while the container is going away, so there is nothing left to exec
	// into afterwards. The `z` relabel is required on an SELinux-enforcing host
	// (the lane is Fedora); without it the container is denied the write.
	let yaml = format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - {out}:/out:z\n    post_start:\n      - command: [\"sh\", \"-c\", \"echo post-start >> /out/hooks\"]\n    pre_stop:\n      - command: [\"sh\", \"-c\", \"echo pre-stop >> /out/hooks\"]\n",
		out = dir.path().display()
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	let after_up = fs::read_to_string(dir.path().join("hooks")).unwrap_or_default();
	engine.down(&file).await.unwrap();
	let after_down = fs::read_to_string(dir.path().join("hooks")).unwrap_or_default();

	assert_eq!(
		after_up.trim(),
		"post-start",
		"post_start did not run, or ran more than once"
	);
	assert_eq!(
		after_down.trim(),
		"post-start\npre-stop",
		"pre_stop did not run on down"
	);
}

// ---------------------------------------------------------------------------
// Health / depends_on
// ---------------------------------------------------------------------------

#[tokio::test]
async fn depends_on_service_healthy() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("hlt");
	let dir = tempfile::tempdir().unwrap();
	let engine = Engine::new(client, proj.clone());
	// `CMD true` is healthy from the first probe, so it cannot show whether web
	// waited. db's healthcheck now depends on work db does after a delay, and
	// web's first act is to read what db left, so web starting early finds
	// nothing.
	//
	// Two constraints pull against each other here. The delay has to exceed the
	// time it takes to create web's container, or the mutation that empties the
	// readiness map still leaves this green (see depends_on_service_completed).
	// And `retries` has to outlast that same delay, or db is declared unhealthy
	// before it writes and `up` fails before any assertion runs — which is what
	// 12s against 10 retries at a 1s interval did.
	let yaml = format!(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"sleep 12; echo db-ready > /out/ready; sleep infinity\"]\n    volumes:\n      - {out}:/out:z\n    healthcheck:\n      test: [\"CMD\", \"test\", \"-f\", \"/out/ready\"]\n      interval: 1s\n      timeout: 2s\n      retries: 30\n      start_period: 0s\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"cat /out/ready > /out/web-saw 2>/dev/null; sleep infinity\"]\n    volumes:\n      - {out}:/out:z\n    depends_on:\n      db:\n        condition: service_healthy\n",
		out = dir.path().display()
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	let saw = poll_host_file(dir.path().join("web-saw"), "db-ready", 30).await;
	engine.down(&file).await.unwrap();

	assert!(
		saw,
		"web started before db reported healthy, so service_healthy was not waited on"
	);
}

#[tokio::test]
async fn depends_on_service_healthy_with_default_timeout() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("hltdef");
	let engine = Engine::new(client, proj.clone());
	// db's healthcheck OMITS `timeout`/`retries`. Podman defaults a missing
	// Timeout to 0s (every probe fails "exceeded timeout of 0s"), so without the
	// compose-spec default the db never becomes healthy and this up would hang.
	let file = parse_str(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    healthcheck:\n      test: [\"CMD\", \"true\"]\n      interval: 1s\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      db:\n        condition: service_healthy\n",
	)
	.unwrap();

	// The comment above states the failure mode exactly: without the compose-spec
	// default, Podman treats a missing Timeout as 0s, every probe fails "exceeded
	// timeout of 0s", db never reports healthy and this `up` hangs until the
	// health deadline. A hang is not something a bare `.unwrap()` can report, so
	// bound it.
	let started = std::time::Instant::now();
	engine.up(&file).await.unwrap();
	let elapsed = started.elapsed();
	let mut names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	names.sort();
	engine.down(&file).await.unwrap();

	assert!(
		elapsed < std::time::Duration::from_secs(45),
		"up took {elapsed:?} — a healthcheck with no explicit timeout must get the spec default, not 0s"
	);
	assert_eq!(
		names,
		vec![format!("{proj}-db-1"), format!("{proj}-web-1")],
		"web did not come up behind a healthcheck that omits timeout and retries"
	);
}

#[tokio::test]
async fn depends_on_service_completed() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("cmp");
	let dir = tempfile::tempdir().unwrap();
	let engine = Engine::new(client, proj.clone());
	// Ordering is the contract, and `up` returning Ok says nothing about it. init
	// takes a while and then leaves a file; app's first act is to read that file.
	//
	// The delay has to beat the cost of creating app's container, not just be
	// non-zero. `up` starts services in dependency levels, so init is launched
	// before app whether or not the readiness wait happens — the wait only adds
	// "and has finished". With a 2s delay the mutation that empties the readiness
	// map left this test green, because creating app took longer than that on its
	// own.
	// If app were started before init completed, the read finds nothing and the
	// marker below stays empty — which is exactly what a dropped dependency wait
	// looks like from outside.
	let yaml = format!(
		"services:\n  init:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"sleep 12; echo init-done > /out/order; exit 0\"]\n    volumes:\n      - {out}:/out:z\n  app:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"cat /out/order > /out/app-saw 2>/dev/null; sleep infinity\"]\n    volumes:\n      - {out}:/out:z\n    depends_on:\n      init:\n        condition: service_completed_successfully\n",
		out = dir.path().display()
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	let saw = poll_host_file(dir.path().join("app-saw"), "init-done", 30).await;
	engine.down(&file).await.unwrap();

	assert!(
		saw,
		"app started before init had completed, so service_completed_successfully was not waited on"
	);
}

// Regression: a dependency scaled to >1 has no base-named container, only
// `{base}-1..N`. The readiness wait must target the first replica, not the
// (nonexistent) base name, or `up` 404s and aborts despite the dep completing.
#[tokio::test]
async fn depends_on_scaled_service_completed() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("cmpscale");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  init:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"exit 0\"]\n    deploy:\n      replicas: 2\n  app:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      init:\n        condition: service_completed_successfully\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let mut names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	names.sort();
	engine.down(&file).await.unwrap();

	// The comment above names the regression precisely: a scaled dependency has no
	// container under the base name, so a readiness wait aimed at `init` rather
	// than `init-1` 404s and aborts `up`.
	//
	// Measured: aiming the wait at the base name reddens this test at
	// `up().unwrap()`, not here — so the bare unwrap already covered that
	// regression. What the assertion adds is that both replicas exist and app
	// came up with them, which the unwrap could not distinguish from a run where
	// app was silently skipped. Precision, not new falsifiability.
	assert_eq!(
		names,
		vec![
			format!("{proj}-app-1"),
			format!("{proj}-init-1"),
			format!("{proj}-init-2"),
		],
		"a scaled completed-successfully dependency did not resolve to both replicas plus the dependant"
	);
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

#[tokio::test]
async fn profile_filtered_service_skipped() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("prf");
	let engine = Engine::new(client, proj.clone());
	// "debug" service has profile "debug" — not in active profiles → skipped
	// "web" has no profiles → always runs
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  debug:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    profiles: [\"debug\"]\n",
	)
	.unwrap();

	engine
		.up_with_options(&file, false, &[], &[], false, false, false)
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
		vec![format!("{proj}-web-1")],
		"the profile-gated service was started even though its profile is not active"
	);
}

// ---------------------------------------------------------------------------
// Replicas
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scale_creates_multiple_replicas() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rep");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  worker:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    deploy:\n      replicas: 2\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let mut names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	names.sort();
	engine.down(&file).await.unwrap();

	// Both replicas, and the `-1`/`-2` suffixes rather than a bare base name: a
	// scaled service has no container under the base name, which is the shape
	// that made the readiness wait 404 in depends_on_scaled_service_completed.
	assert_eq!(
		names,
		vec![format!("{proj}-worker-1"), format!("{proj}-worker-2")],
		"replicas: 2 did not produce two suffixed containers"
	);
}

// ---------------------------------------------------------------------------
// Depends-on: service_healthy with no healthcheck
// ---------------------------------------------------------------------------

#[tokio::test]
async fn depends_on_healthy_no_healthcheck_skips_wait() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("hns");
	let engine = Engine::new(client, proj.clone());
	// backend has no healthcheck; frontend depends on it with service_healthy.
	// podup logs a debug message and skips the wait.
	let file = parse_str(
		"services:\n  backend:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  frontend:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      backend:\n        condition: service_healthy\n",
	)
	.unwrap();

	// "Skips the wait" is the claim, and the way it fails is by NOT skipping:
	// waiting on a container that has no healthcheck can only end in the timeout,
	// so the test would take the full health deadline and then error rather than
	// fail. A bound turns that into an assertion.
	let started = std::time::Instant::now();
	engine.up(&file).await.unwrap();
	let elapsed = started.elapsed();
	let mut names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	names.sort();
	engine.down(&file).await.unwrap();

	assert!(
		elapsed < std::time::Duration::from_secs(30),
		"up took {elapsed:?} — a service_healthy dependency with no healthcheck must be skipped, not waited on"
	);
	assert_eq!(
		names,
		vec![format!("{proj}-backend-1"), format!("{proj}-frontend-1")],
		"the dependant did not come up despite the healthcheck-less dependency being skipped"
	);
}

// ---------------------------------------------------------------------------
// PS with port bindings
// ---------------------------------------------------------------------------

/// `Engine::ps` prints and returns `Result<()>`, so the port column it exists to
/// render is not reachable from here. `cli_ps_subcommand` asserts the table, and
/// `stats_flags::cli_port_prints_the_published_binding` asserts the binding
/// itself. Kept because it drives the with-ports branch of the same call.
#[tokio::test]
async fn ps_with_port_bindings() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("psb");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    ports:\n      - \"19100:80\"\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.ps(&file).await.unwrap();
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
// Query: attach_logs streaming and logs stderr
// ---------------------------------------------------------------------------

/// `attach_logs` streams to stdout, so what it carries cannot be read from the
/// library. Unlike `attach_logs_empty_attach_returns`, which asserts a bound
/// because "returns immediately" is checkable without the stream, there is
/// nothing here to bound — this one names the content. No CLI test covers
/// attached output either, so the claim in the name is unverified.
#[tokio::test]
async fn attach_logs_streams_container_output() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("als");
	let engine = Engine::new(client, proj.clone());
	// Container writes to stdout and stderr then exits; attach_logs should
	// stream the output and return when the stream ends (join_all completes).
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo out-line; echo err-line >&2\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.attach_logs(&file).await.unwrap();
	let _ = engine.down(&file).await;
}

/// Same: `logs` prints, and whether a container's stderr is interleaved into it
/// is a property of the output. `cli_logs_subcommand` asserts stdout content and
/// the service prefix, but drives a service that writes only to stdout — so the
/// stderr path this test is named for is asserted nowhere.
#[tokio::test]
async fn logs_with_stderr_output() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("lge");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo error-msg >&2; sleep infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.logs(&file, Some("web"), false).await.unwrap();
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
