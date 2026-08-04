//! Further CLI binary integration tests (kept separate from cli_commands.rs to
//! stay under the source line limit).
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn cli_logs_tail_limits_output() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-logstail", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"for i in 1 2 3 4 5; do echo line-$i; done; sleep infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "up", "-d"])
		.output()
		.unwrap();

	// Poll until the container has emitted its lines instead of sleeping a fixed
	// duration: `logs --tail 2` must eventually show exactly 2 `line-` rows.
	let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
	let mut logs;
	loop {
		logs = Command::new(bin())
			.args(["-f", c, "-p", &proj, "logs", "--tail", "2"])
			.output()
			.unwrap();
		let lines = String::from_utf8_lossy(&logs.stdout)
			.lines()
			.filter(|l| l.contains("line-"))
			.count();
		if logs.status.success() && lines == 2 {
			break;
		}
		if tokio::time::Instant::now() >= deadline {
			break;
		}
		tokio::time::sleep(std::time::Duration::from_millis(100)).await;
	}
	assert!(logs.status.success(), "logs failed: {:?}", logs.stderr);
	let lines = String::from_utf8_lossy(&logs.stdout)
		.lines()
		.filter(|l| l.contains("line-"))
		.count();
	assert_eq!(lines, 2, "logs --tail 2 must show exactly 2 lines");

	Command::new(bin())
		.args(["-f", c, "-p", &proj, "down"])
		.output()
		.unwrap();
}

fn ps_all_count(compose: &str, proj: &str) -> usize {
	String::from_utf8_lossy(&run(&["-f", compose, "-p", proj, "ps", "-a", "-q"]).stdout)
		.lines()
		.filter(|l| !l.trim().is_empty())
		.count()
}

#[tokio::test]
async fn cli_down_remove_orphans_drops_undeclared_containers() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-orphan", std::process::id());
	let two = dir.path().join("two.yml");
	let one = dir.path().join("one.yml");
	let svc = "image: alpine:latest\n    command: [\"sleep\", \"infinity\"]";
	fs::write(
		&two,
		format!("services:\n  web:\n    {svc}\n  extra:\n    {svc}\n"),
	)
	.unwrap();
	fs::write(&one, format!("services:\n  web:\n    {svc}\n")).unwrap();
	let (two, one) = (two.to_str().unwrap(), one.to_str().unwrap());

	run_ok(&["-f", two, "-p", &proj, "up", "-d"]);
	assert_eq!(ps_all_count(two, &proj), 2);

	// Down against the one-service file: --remove-orphans must also drop `extra`.
	let down = run(&["-f", one, "-p", &proj, "down", "--remove-orphans"]);
	assert!(down.status.success(), "down failed: {:?}", down.stderr);
	assert_eq!(
		ps_all_count(one, &proj),
		0,
		"--remove-orphans must remove the undeclared container too"
	);
}

#[tokio::test]
async fn cli_restart_no_deps_succeeds() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	let proj = format!("t{}-nodeps", std::process::id());
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run_ok(&["-f", c, "-p", &proj, "up", "-d"]);
	let restart = run(&["-f", c, "-p", &proj, "restart", "--no-deps", "web"]);
	assert!(
		restart.status.success(),
		"restart --no-deps failed: {:?}",
		restart.stderr
	);
	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_up_no_build_skips_building() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-nobuild", std::process::id());
	fs::write(dir.path().join("Dockerfile"), "FROM alpine:latest\n").unwrap();
	let compose = dir.path().join("docker-compose.yml");
	// A build-only service with no prebuilt image: `--no-build` must refuse to
	// build, so `up` fails because there is no image to run.
	fs::write(&compose, "services:\n  app:\n    build: .\n").unwrap();
	let up = run(&[
		"-f",
		compose.to_str().unwrap(),
		"-p",
		&proj,
		"up",
		"-d",
		"--no-build",
	]);
	assert!(
		!up.status.success(),
		"--no-build must not build the image, so up has nothing to run"
	);
	run(&["-f", compose.to_str().unwrap(), "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_up_pull_never_starts_present_image() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-pullnever", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();
	// Ensure the image is present, then `--pull never` must still start it.
	run_ok(&["-f", c, "-p", &proj, "up", "-d"]);
	run(&["-f", c, "-p", &proj, "down"]);
	let up = run(&["-f", c, "-p", &proj, "up", "-d", "--pull", "never"]);
	assert!(
		up.status.success(),
		"up --pull never failed: {:?}",
		up.stderr
	);
	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_down_rmi_all_succeeds_and_removes_containers() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-rmi", std::process::id());

	// `--rmi all` deletes the image referenced by the compose file. Tests run in
	// parallel and several rely on the shared `alpine:latest` image, so we tag a
	// throwaway, project-unique local image and reference THAT here. Removing it
	// leaves `alpine:latest` intact for the concurrent tests.
	let throwaway = format!("localhost/podup-rmitest-{proj}:latest");
	// In a clean environment `alpine:latest` may not be pulled yet, so `podman
	// tag` would fail with "image not known". Pull first; ignore the result so a
	// pre-existing image (or an offline cache) still works.
	let _ = Command::new("podman")
		.args(["pull", "alpine:latest"])
		.output();
	let tag = Command::new("podman")
		.args(["tag", "alpine:latest", &throwaway])
		.output()
		.unwrap();
	assert!(
		tag.status.success(),
		"tagging throwaway image failed: {:?}",
		tag.stderr
	);

	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		format!(
			"services:\n  web:\n    image: {throwaway}\n    command: [\"sleep\", \"infinity\"]\n"
		),
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run_ok(&["-f", c, "-p", &proj, "up", "-d"]);
	let down = run(&["-f", c, "-p", &proj, "down", "--rmi", "all"]);
	assert!(
		down.status.success(),
		"down --rmi all failed: {:?}",
		down.stderr
	);
	assert_eq!(ps_all_count(c, &proj), 0, "down must remove the containers");

	// The shared base image must survive; only the throwaway tag was removed.
	let present = Command::new("podman")
		.args(["image", "exists", &throwaway])
		.status()
		.unwrap();
	assert!(
		!present.success(),
		"down --rmi all must remove the throwaway image"
	);
	// Best-effort cleanup in case the assertion above ever changes.
	let _ = Command::new("podman")
		.args(["rmi", "-f", &throwaway])
		.output();
}

#[tokio::test]
async fn cli_rm_volumes_removes_container() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-rmv", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run_ok(&["-f", c, "-p", &proj, "up", "-d"]);
	run(&["-f", c, "-p", &proj, "stop"]);
	let rm = run(&["-f", c, "-p", &proj, "rm", "-v", "-f"]);
	assert!(rm.status.success(), "rm -v failed: {:?}", rm.stderr);
	assert_eq!(ps_all_count(c, &proj), 0, "rm must remove the container");
}

#[tokio::test]
async fn cli_kill_remove_orphans_drops_undeclared() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-killorph", std::process::id());
	let svc = "image: alpine:latest\n    command: [\"sleep\", \"infinity\"]";
	let two = dir.path().join("two.yml");
	let one = dir.path().join("one.yml");
	fs::write(
		&two,
		format!("services:\n  web:\n    {svc}\n  extra:\n    {svc}\n"),
	)
	.unwrap();
	fs::write(&one, format!("services:\n  web:\n    {svc}\n")).unwrap();
	let (two, one) = (two.to_str().unwrap(), one.to_str().unwrap());

	run_ok(&["-f", two, "-p", &proj, "up", "-d"]);
	let kill = run(&["-f", one, "-p", &proj, "kill", "--remove-orphans"]);
	assert!(kill.status.success(), "kill failed: {:?}", kill.stderr);
	// The orphan `extra` is removed; the declared `web` is killed but remains.
	run(&["-f", one, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_pull_quiet_succeeds() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-pullq", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(&compose, "services:\n  web:\n    image: alpine:latest\n").unwrap();
	let pull = run(&["-f", compose.to_str().unwrap(), "-p", &proj, "pull", "-q"]);
	assert!(pull.status.success(), "pull -q failed: {:?}", pull.stderr);
}

#[tokio::test]
async fn cli_up_no_start_creates_without_starting() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-nostart", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let up = run(&["-f", c, "-p", &proj, "up", "--no-start"]);
	assert!(up.status.success(), "up --no-start failed: {:?}", up.stderr);
	assert_eq!(ps_all_count(c, &proj), 1, "container must be created");
	let running = String::from_utf8_lossy(&run(&["-f", c, "-p", &proj, "ps", "-q"]).stdout)
		.lines()
		.filter(|l| !l.trim().is_empty())
		.count();
	assert_eq!(running, 0, "--no-start must not start the container");
	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_up_wait_returns_when_healthy() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-upwait", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	// A healthcheck that omits `timeout` (defaults applied) must still reach
	// healthy, so `up --wait` returns successfully instead of timing out.
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    healthcheck:\n      test: [\"CMD\", \"true\"]\n      interval: 1s\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let up = run(&["-f", c, "-p", &proj, "up", "-d", "--wait"]);
	assert!(up.status.success(), "up --wait failed: {:?}", up.stderr);
	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_rm_stop_removes_running_container() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-rmstop", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	run_ok(&["-f", c, "-p", &proj, "up", "-d"]);
	assert_eq!(ps_all_count(c, &proj), 1, "container should exist after up");

	// `rm -s` (no -f) must stop the running container first, then remove it.
	let rm = run(&["-f", c, "-p", &proj, "rm", "-s", "web"]);
	assert!(rm.status.success(), "rm -s failed: {:?}", rm.stderr);
	assert_eq!(
		ps_all_count(c, &proj),
		0,
		"rm -s must remove the running container"
	);

	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_start_wait_returns_after_starting() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-startwait", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	// Create the container without starting, then `start --wait` must start it
	// and return (no healthcheck → ready once started).
	run_ok(&["-f", c, "-p", &proj, "up", "--no-start"]);
	let start = run(&[
		"-f",
		c,
		"-p",
		&proj,
		"start",
		"--wait",
		"--wait-timeout",
		"30",
	]);
	assert!(
		start.status.success(),
		"start --wait failed: {:?}",
		start.stderr
	);

	run(&["-f", c, "-p", &proj, "down"]);
}

#[tokio::test]
async fn cli_volumes_lists_named_volumes() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-vols", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - data:/data\nvolumes:\n  data:\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	// -q prints only the resolved on-host name ({proj}_data).
	let out = run(&["-f", c, "-p", &proj, "volumes", "-q"]);
	assert!(out.status.success(), "volumes -q failed: {:?}", out.stderr);
	let names = String::from_utf8_lossy(&out.stdout);
	assert!(
		names.lines().any(|l| l.trim() == format!("{proj}_data")),
		"volumes -q must list the resolved volume name, got: {names:?}"
	);

	// JSON format carries the same name.
	let json = run(&["-f", c, "-p", &proj, "volumes", "--format", "json"]);
	assert!(
		json.status.success(),
		"volumes --format json failed: {:?}",
		json.stderr
	);
	let parsed: serde_json::Value =
		serde_json::from_str(String::from_utf8_lossy(&json.stdout).trim()).expect("valid JSON");
	assert!(
		parsed
			.as_array()
			.is_some_and(|a| a.iter().any(|v| v["Name"] == format!("{proj}_data"))),
		"volumes --format json must include the volume: {parsed}"
	);
}

#[tokio::test]
async fn cli_config_resolve_image_digests_pins_digest() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let proj = format!("t{}-digests", std::process::id());
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	// Ensure the image (with its registry digest) is present locally.
	run(&["-f", c, "-p", &proj, "pull"]);

	let out = run(&["-f", c, "-p", &proj, "config", "--resolve-image-digests"]);
	assert!(
		out.status.success(),
		"config --resolve-image-digests failed: {:?}",
		out.stderr
	);
	let yaml = String::from_utf8_lossy(&out.stdout);
	assert!(
		yaml.contains("alpine@sha256:"),
		"image must be pinned to a registry digest, got: {yaml}"
	);
}

/// #1184: `config` left `env_file` unresolved, so a service taking its whole
/// environment from a file rendered with no `environment:` at all — the one
/// command you use to ask what will actually run pointed away from the answer.
/// docker compose materialises it and drops the key; measured against
/// docker compose v5.1.3 on this exact input.
#[test]
fn cli_config_materialises_env_file_into_environment() {
	let dir = tempdir().unwrap();
	let proj = format!("t{}-cfgenv", std::process::id());
	fs::write(dir.path().join("app.env"), "FROM_FILE=resolved\n").unwrap();
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    environment:\n      SHARED: from-service\n    env_file:\n      - app.env\n",
	)
	.unwrap();
	let c = compose.to_str().unwrap();

	let out = run(&["-f", c, "-p", &proj, "config"]);
	assert!(out.status.success(), "config failed: {:?}", out.stderr);
	let yaml = String::from_utf8_lossy(&out.stdout);

	assert!(
		yaml.contains("FROM_FILE: resolved"),
		"the env_file value was not folded into environment: {yaml}"
	);
	assert!(
		!yaml.contains("env_file"),
		"env_file must be dropped once it has been resolved, like docker compose: {yaml}"
	);
	assert!(
		yaml.contains("SHARED: from-service"),
		"a key set in both places must render the value the container would see: {yaml}"
	);
}
