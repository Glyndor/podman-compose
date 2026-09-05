//! Engine integration tests (split for the source line limit).
use super::*;

// Health: non-zero exit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_completed_nonzero_error() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("cne");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  init:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"exit 1\"]\n  app:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      init:\n        condition: service_completed_successfully\n",
	)
	.unwrap();

	// up() propagates the non-zero exit error from wait_completed
	let err = engine.up(&file).await.unwrap_err();
	assert!(
		matches!(err, podup::ComposeError::HealthCheckTimeout(_)),
		"expected HealthCheckTimeout, got: {err}"
	);
	let _ = engine.down(&file).await;
}

// ---------------------------------------------------------------------------
// Profiles: COMPOSE_PROFILES env var path
// ---------------------------------------------------------------------------

#[test]
fn active_profiles_via_env() {
	let rt = tokio::runtime::Runtime::new().unwrap();
	// Set COMPOSE_PROFILES so active_profiles_set reads it (covers profiles.rs L15-19)
	temp_env::with_var("COMPOSE_PROFILES", Some("prod"), || {
		rt.block_on(async {
			let client = match podman().await {
				Some(d) => d,
				None => return,
			};
			let proj = proj("apv");
			let engine = Engine::new(client, proj.clone());
			// "debug" service has profile "debug" — not in "prod" → skipped
			let file = parse_str(
				"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  debug:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    profiles: [\"debug\"]\n",
			)
			.unwrap();
			// Pass empty active_profiles slice so it falls back to COMPOSE_PROFILES env
			engine
				.up_with_options(&file, false, &[], &[], false, false, false, false)
				.await
				.unwrap();
			let mut names = engine
				.test_project_container_names()
				.await
				.unwrap_or_default();
			names.sort();
			engine.down(&file).await.unwrap();

			// COMPOSE_PROFILES=prod must not enable the "debug" profile. Reading it
			// and then starting everything anyway returned Ok just as happily.
			assert_eq!(
				names,
				vec![format!("{proj}-web-1")],
				"COMPOSE_PROFILES=prod started the debug-profiled service"
			);
		});
	});
}

// ---------------------------------------------------------------------------
// Health: wait_healthy timeout and wait_completed polling
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_healthy_times_out() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("wht");
	let engine = Engine::new(client, proj.clone());
	// CMD false always fails; retries:1 means wait_healthy exhausts quickly
	// Covers health.rs L42-43 (loop body closing braces) and L47 (timeout Err)
	let file = parse_str(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    healthcheck:\n      test: [\"CMD\", \"false\"]\n      interval: 1s\n      timeout: 1s\n      retries: 1\n      start_period: 0s\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      db:\n        condition: service_healthy\n",
	)
	.unwrap();

	let err = engine.up(&file).await.unwrap_err();
	assert!(
		matches!(err, podup::ComposeError::HealthCheckTimeout(_)),
		"expected HealthCheckTimeout, got: {err}"
	);
	let _ = engine.down(&file).await;
}

#[tokio::test]
async fn wait_completed_polling() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("wcp");
	let engine = Engine::new(client, proj.clone());
	// init takes a while before exiting, so the first poll sees it running and the
	// polling loop is exercised rather than short-circuited. The delay has to beat
	// the cost of creating app's container, or `up` would appear ordered even with
	// the wait removed — the reason 2s was not enough in
	// resources_health::depends_on_service_completed.
	// The bind uses an absolute path, so no base_dir is needed.
	let dir = tempfile::tempdir().unwrap();
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
		"app started before init completed, so the polling wait was not honoured"
	);
}

// ---------------------------------------------------------------------------
// External (Podman-native) config injection
// ---------------------------------------------------------------------------

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn external_config_injected_into_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("incfg");
	let secret_name = format!("{proj}-cfg");

	// External configs are backed by Podman secrets too; create one out-of-band.
	let dir = tempfile::tempdir().unwrap();
	let src = dir.path().join("cfg");
	fs::write(&src, b"native-config-value").unwrap();
	match std::process::Command::new("podman")
		.args(["secret", "create", &secret_name, src.to_str().unwrap()])
		.status()
	{
		Ok(s) if s.success() => {}
		_ => return,
	}

	// A long-form absolute target must land the config at that exact path, not
	// under /run/secrets — the config-specific behaviour.
	let yaml = format!(
		"services:\n  app:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    configs:\n      - source: cfg\n        target: /etc/app.conf\nconfigs:\n  cfg:\n    external: true\n    name: {secret_name}\n"
	);
	let file = parse_str(&yaml).unwrap();
	let engine = Engine::new(client, proj.clone());
	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-app-1");
	let out = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/etc/app.conf".into()])
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let _ = std::process::Command::new("podman")
		.args(["secret", "rm", &secret_name])
		.status();

	assert!(
		out.contains("native-config-value"),
		"external config was not injected at /etc/app.conf: {out:?}"
	);
}

// ---------------------------------------------------------------------------
// Container options: expose with slash, env_file, ulimits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn service_with_expose_proto_and_ulimits() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	// expose "8080/tcp" (with slash) covers container.rs L57 (raw.clone() branch)
	// ulimits covers container.rs L150 (Some(ulimits))
	let proj = proj("seu");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    expose:\n      - \"8080/tcp\"\n    ulimits:\n      nofile:\n        soft: 1024\n        hard: 2048\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Both keys are silently droppable: a container with neither still starts, and
	// `up` returns Ok either way. The ulimits are readable from inside the
	// container; `expose` is container config, so read it out of band.
	let limits = engine
		.test_exec_capture(
			&cname,
			vec!["sh".into(), "-c".into(), "ulimit -Sn; ulimit -Hn".into()],
		)
		.await
		.unwrap_or_default();
	let exposed = String::from_utf8_lossy(
		&std::process::Command::new("podman")
			.args([
				"inspect",
				&cname,
				"--format",
				"{{json .Config.ExposedPorts}}",
			])
			.output()
			.expect("podman inspect")
			.stdout,
	)
	.trim()
	.to_string();
	engine.down(&file).await.unwrap();

	assert_eq!(
		limits.trim(),
		"1024\n2048",
		"the soft and hard nofile limits did not reach the container"
	);
	// The `/tcp` suffix is the point: `expose: "8080/tcp"` takes the raw-string
	// branch, and a parser that dropped the protocol would still produce a port.
	assert!(
		exposed.contains("8080/tcp"),
		"expose did not reach the container config with its protocol: {exposed:?}"
	);
}

#[tokio::test]
async fn env_file_loaded() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(dir.path().join("test.env"), b"MYVAR=hello\n").unwrap();

	let proj = proj("evf");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    env_file:\n      - test.env\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// The variable has to reach the container's environment. Reaching the line
	// that loads the file is not the same thing: this test used to note which
	// source line it covered and then assert nothing, so it would have passed just
	// as well with `env_file` dropped on the floor.
	let read = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"test \"$MYVAR\" = hello".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await;
	engine.down(&file).await.unwrap();
	assert!(
		read.is_ok(),
		"MYVAR from env_file did not reach the container's environment: {read:?}"
	);
}

// ---------------------------------------------------------------------------
// Lifecycle: target_set skips non-targeted service; dep profile skip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn target_services_skips_non_dep() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("tsk");
	let engine = Engine::new(client, proj.clone());
	// "extra" is not depended upon by web → skipped (lifecycle.rs L56 continue)
	let file = parse_str(
		"services:\n  extra:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine
		.up_with_options(
			&file,
			false,
			&[],
			&["web".to_string()],
			false,
			false,
			false,
			false,
		)
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
		"targeting web started extra as well, which nothing depends on"
	);
}

#[tokio::test]
async fn dep_on_profile_filtered_service() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("dpf");
	let engine = Engine::new(client, proj.clone());
	// podup ACTIVATES a profile-filtered service when a service that is running
	// depends on it, transitively, so a retained service never points at a dropped
	// one. The rationale is in internal/engine/profiles.rs. This comment used to
	// say the opposite — that db is skipped and its dep wait skipped with it —
	// which is what a deliberate design looks like when nothing pins it: the next
	// reader believes the comment over the code.
	let file = parse_str(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    profiles: [\"debug\"]\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      - db\n",
	)
	.unwrap();

	engine
		.up_with_options(&file, false, &[], &[], false, false, false, false)
		.await
		.unwrap();
	let mut names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	names.sort();
	engine.down(&file).await.unwrap();

	// This is a DELIBERATE divergence from docker compose, pinned here so it
	// cannot be reverted by accident. Measured 2026-08-02 against docker-compose
	// v5.1.3 on the same Podman socket: it refuses the project with
	// `service "web" depends on undefined service "db"` — a misleading message,
	// since db is defined and filtered, not undefined.
	//
	// podup is not departing from a standard. The Compose Specification says only
	// that a profiled service starts "if the profile is activated" and is silent
	// on a dependency whose profile is inactive; Docker's own documentation makes
	// it a requirement on the author (same profile, started separately, or
	// unprofiled) rather than a rule the spec imposes. So the reference errors on
	// a situation that is satisfiable, and podup resolves it.
	//
	// If you are here because this looks wrong: read #1276 and
	// docs/docker-migration.md first. Changing it is a behaviour change and a
	// breaking one, not a bug fix.
	assert_eq!(
		names,
		vec![format!("{proj}-db-1"), format!("{proj}-web-1")],
		"the profile-gated dependency was not activated; this divergence is deliberate, see #1276"
	);
}

// ---------------------------------------------------------------------------
// Build: arg with null value (from environment)
// ---------------------------------------------------------------------------

#[test]
fn build_with_env_arg() {
	let rt = tokio::runtime::Runtime::new().unwrap();
	// FROM_ENV has no explicit value → read from environment (build.rs L89 None branch)
	temp_env::with_var("FROM_ENV", Some("test-value"), || {
		rt.block_on(async {
			let client = match podman().await {
				Some(d) => d,
				None => return,
			};
			let dir = tempfile::tempdir().unwrap();
			let proj = proj("bea");
			let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
			let image_tag = format!("podup-test-bea-{}:latest", std::process::id());
			let yaml = format!(
				"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        ARG FROM_ENV\n        RUN echo $$FROM_ENV > /from-env\n      args:\n        FROM_ENV:\n    image: {image_tag}\n    command: [\"sleep\", \"infinity\"]\n"
			);
			let file = parse_str(&yaml).unwrap();

			engine.up(&file).await.unwrap();
			// Same two traps as the build-arg tests in build_images.rs, and this one
			// is the sharper case. A valueless `args: FROM_ENV:` entry is supposed to
			// take its value from the environment, and the single-dollar form the old
			// version used made compose substitute FROM_ENV in the YAML instead — so
			// the value arrived by a completely different route than the one under
			// test, and the ARG never mattered. `RUN echo` then left nothing to
			// contradict it.
			let baked = engine
				.test_exec_capture(
					&format!("{proj}-app-1"),
					vec!["cat".into(), "/from-env".into()],
				)
				.await
				.unwrap_or_default();
			engine.down(&file).await.unwrap();
			let _ = std::process::Command::new("podman")
				.args(["rmi", "-f", &image_tag])
				.status();

			assert_eq!(
				baked.trim(),
				"test-value",
				"a valueless build arg did not pick up the environment variable"
			);
		});
	});
}

// label_file: load tests moved to label_file_safety.rs (#1361).
