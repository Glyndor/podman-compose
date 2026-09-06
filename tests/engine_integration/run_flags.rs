//! `run` flag-parity integration tests (split for the source line limit).
//!
//! The CLI-only run flags are threaded through `Engine::with_run_overrides`
//! (the public `RunOptions` API stays frozen at 1.0), so each test builds an
//! engine carrying the overrides under test.
use super::*;

// Seven assertions in this suite deliberately do not print the `Result` they
// assert on, while others still do. `rust/cleartext-logging` takes its sources
// from the NAME of the called function, so `create_project_secrets` is a taint
// source and those seven were the sinks it reached through `?`. Nothing
// syntactic separates them from the rest, so this note is what keeps the
// inconsistency from reading as an oversight somebody tidies up. #1599.

#[tokio::test]
async fn engine_run_applies_user_workdir_env_and_entrypoint() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("ruwe");
	let engine = Engine::new(client, proj.clone()).with_run_overrides(podup::RunOverrides::new(
		Some("0".to_string()),
		Some("/tmp".to_string()),
		Some("sh".to_string()),
		Vec::new(),
		Vec::new(),
		false,
		false,
	));
	let file = parse_str("services:\n  job:\n    image: alpine:latest\n").unwrap();

	// --entrypoint sh, cmd is its args; -u root, -w /tmp, -e MARK=ok. The command
	// only exits 0 when the working dir, user id and env override all took effect.
	let result = engine
		.run(
			&file,
			"job",
			podup::RunOptions::new(
				vec![
					"-c".to_string(),
					"test \"$(pwd)\" = /tmp && test \"$(id -u)\" = 0 && test \"$MARK\" = ok"
						.to_string(),
				],
				true,
				false,
				vec!["MARK=ok".to_string()],
				None,
				false,
			),
		)
		.await;
	assert!(
		result.is_ok(),
		"run with user/workdir/env/entrypoint failed"
	);
}

#[tokio::test]
async fn engine_run_applies_volume_publish_and_interactive() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(dir.path().join("marker.txt"), b"present").unwrap();
	// `z` relabels the host directory so a confined container can read it. Without
	// it this test fails on every SELinux-enforcing host, measured on Fedora 44 +
	// Podman 5.8.1, where a `user_tmp_t` file bound read-only gives the container
	// `cat: can't open '/mnt/in/marker.txt': Permission denied`. Plain `podman run`
	// behaves identically, so the denial was never podup's; the mount just needed
	// the relabel a user on such a host writes too. Where SELinux is off the option
	// is a no-op, so the same spec covers both.
	let mount = format!("{}:/mnt/in:ro,z", dir.path().to_str().unwrap());

	let proj = proj("rvp");
	let engine = Engine::new(client, proj.clone()).with_run_overrides(podup::RunOverrides::new(
		None,
		None,
		None,
		vec![mount],
		vec!["127.0.0.1:0:9".to_string()],
		true,
		false,
	));
	let file = parse_str("services:\n  job:\n    image: alpine:latest\n").unwrap();

	// -v bind-mounts the host dir, -i keeps stdin open, -p publishes an ad-hoc
	// port. The command only exits 0 if the mounted marker is readable.
	let result = engine
		.run(
			&file,
			"job",
			podup::RunOptions::new(
				vec![
					"sh".to_string(),
					"-c".to_string(),
					"test \"$(cat /mnt/in/marker.txt)\" = present".to_string(),
				],
				true,
				false,
				Vec::new(),
				None,
				false,
			),
		)
		.await;
	assert!(result.is_ok(), "run with volume/publish/interactive failed");
}

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn engine_run_no_deps_skips_dependency_startup() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rnd");
	let engine = Engine::new(client, proj.clone()).with_run_overrides(podup::RunOverrides::new(
		None,
		None,
		None,
		Vec::new(),
		Vec::new(),
		false,
		true,
	));
	let file = parse_str(
		"services:\n  job:\n    image: alpine:latest\n    depends_on:\n      - dep\n  dep:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	// With --no-deps the dependency must not be started.
	let result = engine
		.run(
			&file,
			"job",
			podup::RunOptions::new(
				vec!["true".to_string()],
				true,
				false,
				Vec::new(),
				None,
				false,
			),
		)
		.await;
	let names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	let dep_present = names.iter().any(|n| n.contains("-dep"));
	engine.down(&file).await.unwrap();
	assert!(result.is_ok(), "run --no-deps failed");
	assert!(
		!dep_present,
		"dependency container created despite --no-deps: {names:?}"
	);
}

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn engine_run_starts_dependencies_by_default() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rwd");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  job:\n    image: alpine:latest\n    depends_on:\n      - dep\n  dep:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let result = engine
		.run(
			&file,
			"job",
			podup::RunOptions::new(
				vec!["true".to_string()],
				true,
				false,
				Vec::new(),
				None,
				false,
			),
		)
		.await;
	let names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	let dep_present = names.iter().any(|n| n.contains("-dep"));
	engine.down(&file).await.unwrap();
	assert!(result.is_ok(), "run with deps failed");
	assert!(
		dep_present,
		"dependency was not started by default: {names:?}"
	);
}
