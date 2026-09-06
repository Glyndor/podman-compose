//! Quadlet-mode autostart against real Podman **and real systemd `--user`**.
//!
//! The install/uninstall/rebuild flow shipped covered by unit tests only (a
//! fake `SystemCtl` plus the pure renderer tests) so nothing exercised the part
//! that actually breaks: units written to the systemd directory, generated,
//! started, and the containers coming up. #1091 is what that gap costs. Its bug
//! (a relative `EnvironmentFile=` resolving against the unit directory once
//! installed) is invisible to every renderer test, because the rendered text was
//! exactly what the test expected; it only fails when systemd reads it.
//!
//! **These tests touch the caller's real systemd.** `systemd --user` reads
//! `XDG_CONFIG_HOME` from the manager process, set at login, so a test process
//! cannot redirect it the way the unit tests do: the units really are written to
//! `~/.config/containers/systemd/` and really are started. Two consequences,
//! both load-bearing:
//!
//! * every test uses a per-process project name, so a run cannot collide with
//!   another run, with the developer's own stacks, or with a sibling test;
//! * cleanup runs from `Drop`, so a panic mid-test cannot leave an enabled unit
//!   behind. That is not tidiness: a leaked quadlet unit starts its container
//!   on the user's next boot.
//!
//! They skip when Podman or `systemd --user` is unreachable, which is every
//! non-Linux runner and the main CI job; the nested-virt lane is where they run.

use std::path::PathBuf;
use std::process::Command;

use super::{bin, proj};

/// Whether a usable `systemd --user` session is reachable. `systemctl --user`
/// needs a session bus, so this is false in a plain container and on any
/// non-Linux host.
fn systemd_user_available() -> bool {
	Command::new("systemctl")
		.args(["--user", "show", "--property=Version"])
		.output()
		.map(|o| o.status.success())
		.unwrap_or(false)
}

/// `${XDG_CONFIG_HOME:-~/.config}/containers/systemd`, the directory Quadlet
/// reads. Resolved the same way podup resolves it.
fn quadlet_dir() -> PathBuf {
	let base = std::env::var_os("XDG_CONFIG_HOME")
		.filter(|s| !s.is_empty())
		.map(PathBuf::from)
		.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
		.unwrap_or_default();
	base.join("containers").join("systemd")
}

/// Runs `podup` with `-f <compose> -p <project>` and returns the exit status.
fn podup(compose: &std::path::Path, project: &str, args: &[&str]) -> std::process::Output {
	Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", project])
		.args(args)
		.output()
		.expect("run podup")
}

fn unit_active(service: &str) -> bool {
	Command::new("systemctl")
		.args(["--user", "is-active", "--quiet", service])
		.status()
		.map(|s| s.success())
		.unwrap_or(false)
}

/// Tears the install down however the test ends.
///
/// A quadlet unit left installed is not inert: it starts its container at the
/// next boot. So uninstall runs from `Drop`, which a panicking assertion cannot
/// skip, and `down -v` follows to reclaim the containers and volumes.
struct Installed {
	compose: PathBuf,
	project: String,
}

impl Drop for Installed {
	fn drop(&mut self) {
		let _ = podup(&self.compose, &self.project, &["autostart", "uninstall"]);
		let _ = podup(&self.compose, &self.project, &["down", "-v"]);
	}
}

/// Write a one-service compose file into a fresh temp dir, and return both.
fn fixture(tag: &str) -> (tempfile::TempDir, PathBuf) {
	let dir = tempfile::tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	std::fs::write(
		&compose,
		format!(
			"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n\
			 \n# {tag}\n"
		),
	)
	.unwrap();
	(dir, compose)
}

/// Install writes the units, systemd generates and starts them, and uninstall
/// leaves nothing behind: the whole round trip, through the real generator.
#[tokio::test]
async fn quadlet_autostart_installs_starts_and_uninstalls() {
	if super::podman().await.is_none() || !systemd_user_available() {
		return;
	}
	let project = proj("qauto");
	let (_dir, compose) = fixture(&project);
	let guard = Installed {
		compose: compose.clone(),
		project: project.clone(),
	};

	let out = podup(
		&compose,
		&project,
		&["autostart", "install", "--mode", "quadlet"],
	);
	assert!(
		out.status.success(),
		"install failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let unit = quadlet_dir().join(format!("{project}-web.container"));
	assert!(unit.is_file(), "unit not written to {}", unit.display());

	// The generator runs on `daemon-reload`, so the service exists only if the
	// unit it produced was valid: a unit Quadlet rejects silently yields no
	// service at all, which is the failure mode a renderer test cannot see.
	let service = format!("{project}-web.service");
	let mut active = false;
	for _ in 0..30 {
		if unit_active(&service) {
			active = true;
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(500));
	}
	assert!(active, "{service} never became active");

	drop(guard);

	assert!(!unit.exists(), "uninstall left {} behind", unit.display());
	assert!(!unit_active(&service), "uninstall left {service} running");
}

/// #1091 as a regression test: a service with a relative `env_file` must come up
/// with the variable set.
///
/// This is the one the renderer could not catch. The generated
/// `EnvironmentFile=` was exactly the string the unit tests asserted; it was
/// systemd resolving it against the unit's own directory, not the compose
/// file's, that made `--env-file` fatal on a path that does not exist there, so
/// the container never started.
#[tokio::test]
async fn quadlet_autostart_resolves_a_relative_env_file() {
	if super::podman().await.is_none() || !systemd_user_available() {
		return;
	}
	let project = proj("qenv");
	let dir = tempfile::tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	std::fs::write(dir.path().join(".env"), b"PODUP_MARKER=present\n").unwrap();
	std::fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    env_file:\n      - .env\n    \
		 command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let _guard = Installed {
		compose: compose.clone(),
		project: project.clone(),
	};

	let out = podup(
		&compose,
		&project,
		&["autostart", "install", "--mode", "quadlet"],
	);
	assert!(
		out.status.success(),
		"install failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);

	let service = format!("{project}-web.service");
	let mut active = false;
	for _ in 0..30 {
		if unit_active(&service) {
			active = true;
			break;
		}
		std::thread::sleep(std::time::Duration::from_millis(500));
	}
	assert!(
		active,
		"{service} never became active: a relative env_file that resolves against \
		 the unit directory makes --env-file fatal, so the container cannot start"
	);

	let env = Command::new("podman")
		.args([
			"exec",
			&format!("{project}-web"),
			"printenv",
			"PODUP_MARKER",
		])
		.output()
		.expect("run podman exec");
	assert_eq!(
		String::from_utf8_lossy(&env.stdout).trim(),
		"present",
		"the env_file entry did not reach the container"
	);
}
