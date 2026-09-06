//! `include:` and `extends:` run end to end.
//!
//! Both are covered thoroughly at the parser level (`tests/parse/include.rs`,
//! `tests/parse/extends.rs`), which proves the merged model is right. Nothing
//! proved a project using either actually comes **up**; the 2026-06-25 empirical
//! sweep listed them among the surfaces it never reached.
//!
//! These go through the CLI rather than `Engine`, because the part with no
//! parser coverage is path resolution against the file doing the including or
//! extending. #1091 was exactly that shape: a relative `env_file` resolved
//! against the wrong directory, so the unit that read it was correct and the
//! container still never started.
use std::fs;
use std::process::Command;
use tempfile::tempdir;

use super::*;

/// Run the built `podup` against compose file `c` / project `proj`.
fn podup(c: &str, proj: &str, args: &[&str]) -> std::process::Output {
	Command::new(bin())
		.args(["-f", c, "-p", proj])
		.args(args)
		.output()
		.expect("run podup")
}

/// `sh -c <script>` inside `service`, as an exit-code assertion.
fn exec_ok(c: &str, proj: &str, service: &str, script: &str) -> bool {
	podup(c, proj, &["exec", "-T", service, "sh", "-c", script])
		.status
		.success()
}

#[tokio::test]
async fn include_brings_up_the_included_service() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	// The included file lives in a subdirectory, so its own relative `env_file`
	// only resolves if paths are anchored to the included file rather than to the
	// including one.
	let sub = dir.path().join("parts");
	fs::create_dir(&sub).unwrap();
	fs::write(sub.join("db.env"), b"INCLUDED_VAR=from-included-env\n").unwrap();
	fs::write(
		sub.join("db.yml"),
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    env_file:\n      - db.env\n",
	)
	.unwrap();

	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"include:\n  - parts/db.yml\nservices:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let c = compose.to_str().unwrap();
	let proj = proj("incl");
	let up = podup(c, &proj, &["up", "-d"]);

	let db_env = exec_ok(c, &proj, "db", "test \"$INCLUDED_VAR\" = from-included-env");
	let ps = podup(c, &proj, &["ps"]);
	let listing = String::from_utf8_lossy(&ps.stdout).to_string();
	podup(c, &proj, &["down", "-v"]);

	assert!(
		up.status.success(),
		"up failed for a project using include: {}",
		String::from_utf8_lossy(&up.stderr)
	);
	assert!(
		listing.contains("-db-1"),
		"the included service never started: {listing}"
	);
	assert!(
		db_env,
		"the included file's relative env_file did not resolve against its own directory"
	);
}

#[tokio::test]
async fn extends_in_the_same_file_carries_the_base_config() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	// `child` inherits BASE from the base service and overrides OWN, so a container
	// that only has one of the two proves the merge is half-applied.
	fs::write(
		&compose,
		"services:\n  base:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    environment:\n      BASE_VAR: from-base\n      OWN_VAR: from-base\n  child:\n    extends:\n      service: base\n    environment:\n      OWN_VAR: from-child\n",
	)
	.unwrap();

	let c = compose.to_str().unwrap();
	let proj = proj("extsame");
	let up = podup(c, &proj, &["up", "-d"]);

	let merged = exec_ok(
		c,
		&proj,
		"child",
		"test \"$BASE_VAR\" = from-base && test \"$OWN_VAR\" = from-child",
	);
	podup(c, &proj, &["down", "-v"]);

	assert!(
		up.status.success(),
		"up failed for a project using extends: {}",
		String::from_utf8_lossy(&up.stderr)
	);
	assert!(
		merged,
		"the extending service did not inherit the base environment and override its own key"
	);
}

#[tokio::test]
async fn extends_across_files_anchors_relative_paths_to_the_extended_file() {
	if super::podman().await.is_none() {
		return;
	}
	let dir = tempdir().unwrap();
	// The base lives in a subdirectory and names its env_file relatively. The
	// compose spec anchors that path to the file the base is defined in, not to
	// the one extending it, the runtime half of what tests/parse/extends.rs
	// checks on the parsed model.
	let sub = dir.path().join("common");
	fs::create_dir(&sub).unwrap();
	fs::write(sub.join("base.env"), b"BASE_ENV_VAR=from-base-dir\n").unwrap();
	fs::write(
		sub.join("base.yml"),
		"services:\n  app:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    env_file:\n      - base.env\n",
	)
	.unwrap();

	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  web:\n    extends:\n      file: common/base.yml\n      service: app\n    environment:\n      LOCAL_VAR: from-child\n",
	)
	.unwrap();

	let c = compose.to_str().unwrap();
	let proj = proj("extfile");
	let up = podup(c, &proj, &["up", "-d"]);

	let merged = exec_ok(
		c,
		&proj,
		"web",
		"test \"$BASE_ENV_VAR\" = from-base-dir && test \"$LOCAL_VAR\" = from-child",
	);
	podup(c, &proj, &["down", "-v"]);

	assert!(
		up.status.success(),
		"up failed for extends across files: {}",
		String::from_utf8_lossy(&up.stderr)
	);
	assert!(
		merged,
		"the extended file's relative env_file did not resolve against its own directory"
	);
}
