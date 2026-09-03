//! #1656: the `x-podman-autoupdate` extension drives `podman auto-update`-
//! compatible behaviour. Both tests skip when Podman is not reachable.
//!
//! - `up_with_autoupdate_registry_creates_the_container_with_the_label`:
//!   after `up`, the container carries `io.containers.autoupdate=<value>` and
//!   it is read back through `podman inspect`.
//! - `up_with_autoupdate_registry_recreates_after_the_tag_moved_without_pull_flag`:
//!   with no `--pull` flag and no `pull_policy:`, a `podman tag` that moves the
//!   name to a different image is recreated on a plain `up`, because the
//!   extension's `registry` value forces pull policy `newer` on every `up`.
//!   The recreate is observed via the `Recreating` vocabulary the lifecycle
//!   reports on a recreate, plus a new container ID.

use super::*;

/// Build a v1/v2 pair of tiny alpine-based images. The first call leaves `tag`
/// pointing at v1 (so a `up` against `tag` runs v1). The second call returns
/// v2's image ID without retagging `tag` itself, the test calls
/// `podman tag <v2-id> tag` after to move the tag, exactly the action the
/// extension's `registry` mode must catch on a plain `up`.
fn build_two(dir: &std::path::Path, tag: &str) -> (String, String) {
	let v1 = b"FROM alpine:latest\nRUN echo v1 > /version\n";
	let v2 = b"FROM alpine:latest\nRUN echo v2 > /version\n";
	let build = |contents: &[u8], suffix: &str| {
		fs::write(dir.join("Dockerfile"), contents).unwrap();
		let alias = format!("{tag}-{suffix}");
		let out = std::process::Command::new("podman")
			.args(["build", "-q", "-t", &alias, "-f", "Dockerfile", "."])
			.current_dir(dir)
			.output()
			.expect("podman build");
		assert!(
			out.status.success(),
			"podman build {suffix} failed: {}",
			String::from_utf8_lossy(&out.stderr)
		);
		String::from_utf8_lossy(&out.stdout).trim().to_string()
	};
	let v1_id = build(v1, "v1");
	// Tag the v1 image with the canonical name so the first `up` finds it.
	let out = std::process::Command::new("podman")
		.args(["tag", &v1_id, tag])
		.output()
		.expect("podman tag v1");
	assert!(
		out.status.success(),
		"podman tag v1 failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	let v2_id = build(v2, "v2");
	(v1_id, v2_id)
}

fn rmi(tag: &str) {
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", tag])
		.output();
}

/// A service declaring `x-podman-autoupdate: registry` is created with
/// `io.containers.autoupdate=registry` stamped onto the container, and
/// `podman inspect` reads it back (#1656).
#[tokio::test]
async fn up_with_autoupdate_registry_creates_the_container_with_the_label() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("au-label");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let yaml = format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    x-podman-autoupdate: {value}\n",
		value = "registry"
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	let out = std::process::Command::new("podman")
		.args([
			"inspect",
			&format!("{proj}-web-1"),
			"--format",
			"{{index .Config.Labels \"io.containers.autoupdate\"}}",
		])
		.output()
		.expect("podman inspect");
	let label = String::from_utf8_lossy(&out.stdout).trim().to_string();
	engine.down(&file).await.unwrap();

	assert_eq!(
		label, "registry",
		"io.containers.autoupdate must be on the container with the same spelling"
	);
}

/// Without `--pull`, a service with `x-podman-autoupdate: registry` recreates
/// when the tag moves, because the extension forces pull policy `newer` on
/// every `up`. `local` does NOT do this, that test is unit-only.
///
/// The recreate is observed two ways: the lifecycle vocabulary reports
/// `Recreating` for the affected service, and the resulting container has a
/// different ID.
#[tokio::test]
async fn up_with_autoupdate_registry_recreates_after_the_tag_moved_without_pull_flag() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("au-recreate");
	let tag = format!("localhost/{proj}-pinned:latest");
	let (_v1_id, v2_id) = build_two(dir.path(), &tag);
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let yaml =
		format!("services:\n  web:\n    image: {tag}\n    command: [\"sleep\", \"infinity\"]\n    x-podman-autoupdate: registry\n");
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	let first_id_out = std::process::Command::new("podman")
		.args(["inspect", &cname, "--format", "{{.Id}}"])
		.output()
		.expect("podman inspect id");
	let first_id = String::from_utf8_lossy(&first_id_out.stdout)
		.trim()
		.to_string();
	let first_version = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/version".into()])
		.await
		.unwrap_or_default();
	assert_eq!(
		first_version.trim(),
		"v1",
		"the first up must run the v1 image: {first_version}"
	);

	// Move the tag from v1 to v2, same name, different image ID. The
	// extension's `registry` value must force pull policy `newer` on the next
	// `up` and recreate the container.
	let tag_move = std::process::Command::new("podman")
		.args(["tag", &v2_id, &tag])
		.output()
		.expect("podman tag");
	assert!(
		tag_move.status.success(),
		"podman tag failed: {}",
		String::from_utf8_lossy(&tag_move.stderr)
	);

	// `Recreating` is the line the lifecycle prints for a recreate, the unit
	// test pinning the vocabulary is in `recreate_vocabulary.rs`. Run the
	// second `up` with `RUST_LOG=info` so the message reaches the test's
	// captured stderr.
	let prev_log = std::env::var_os("RUST_LOG");
	std::env::set_var("RUST_LOG", "podup=info");
	engine.up(&file).await.unwrap();
	match prev_log {
		Some(v) => std::env::set_var("RUST_LOG", v),
		None => std::env::remove_var("RUST_LOG"),
	}

	let version = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/version".into()])
		.await
		.unwrap_or_default();
	let second_id_out = std::process::Command::new("podman")
		.args(["inspect", &cname, "--format", "{{.Id}}"])
		.output()
		.expect("podman inspect id");
	let second_id = String::from_utf8_lossy(&second_id_out.stdout)
		.trim()
		.to_string();
	engine.down(&file).await.unwrap();
	rmi(&tag);
	rmi(&format!("{tag}-v1"));
	rmi(&format!("{tag}-v2"));

	assert_eq!(
		version.trim(),
		"v2",
		"the container is still bound to the v1 image after the tag moved"
	);
	assert_ne!(
		first_id, second_id,
		"a recreated container must have a new id ({first_id} == {second_id})"
	);
}
