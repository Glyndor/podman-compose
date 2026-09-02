//! `up` keeps or replaces a container by two facts: the config hash, and the
//! image the container is bound to against the image its service resolves to
//! now (#1620). These tests pin the second fact on a real Podman.
//!
//! The marker technique is the one `up_skips_recreate_when_config_unchanged`
//! uses: a file written into the running container survives a skip and is
//! gone after a recreate, so the three outcomes read from inside rather than
//! from a container ID compared by hand. Every service here has no `image:`,
//! so the tag the engine inspects is the derived `{project}-{service}:latest`
//! one, which is the path #1620 measured.

use super::*;

const V1: &[u8] = b"FROM alpine:latest\nRUN echo v1 > /version\n";
const V2: &[u8] = b"FROM alpine:latest\nRUN echo v2 > /version\n";
const YAML: &str = "services:\n  web:\n    build: .\n    command: [\"sleep\", \"infinity\"]\n";

fn write_marker() -> Vec<String> {
	vec!["sh".into(), "-c".into(), "echo marked > /marker".into()]
}

fn read_marker() -> Vec<String> {
	vec!["cat".into(), "/marker".into()]
}

fn rmi(project: &str) {
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", &format!("{project}-web:latest")])
		.output();
}

/// The case #1620 measured: an unchanged `build:` service is left alone. Before
/// the fix it was stopped, removed, created and started on every `up`, bound
/// to exactly the image it already had, and its writable layer went with it.
#[tokio::test]
async fn an_unchanged_build_service_is_left_in_place() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), V1).unwrap();
	let proj = proj("rci-same");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(YAML).unwrap();
	let cname = format!("{proj}-web-1");

	engine.up(&file).await.unwrap();
	engine
		.test_exec_capture(&cname, write_marker())
		.await
		.unwrap();
	engine.up(&file).await.unwrap();
	let after = engine
		.test_exec_capture(&cname, read_marker())
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	rmi(&proj);

	assert_eq!(
		after.trim(),
		"marked",
		"an unchanged build: service was recreated instead of kept"
	);
}

/// The reason the old rule existed, done properly: when the image behind the
/// tag changes and the compose config does not, the container is replaced.
/// The rebuild here is out of band (`build`, then `up`), which is the shape a
/// `build --no-cache` followed by `up` takes. docker compose v5.3.1 recreates
/// on this too.
#[tokio::test]
async fn a_rebuilt_image_recreates_the_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(dir.path().join("Dockerfile"), V1).unwrap();
	let proj = proj("rci-rebuild");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(YAML).unwrap();
	let cname = format!("{proj}-web-1");

	engine.up(&file).await.unwrap();
	engine
		.test_exec_capture(&cname, write_marker())
		.await
		.unwrap();
	fs::write(dir.path().join("Dockerfile"), V2).unwrap();
	engine.build_all(&file, &[]).await.unwrap();
	engine.up(&file).await.unwrap();
	let version = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/version".into()])
		.await
		.unwrap_or_default();
	let marker = engine
		.test_exec_capture(&cname, read_marker())
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	rmi(&proj);

	assert_eq!(
		version.trim(),
		"v2",
		"the container is still bound to the old image after a rebuild"
	);
	assert_ne!(
		marker.trim(),
		"marked",
		"a rebuilt image must replace the container, and the old writable layer with it"
	);
}

/// The same fact from the other direction: an `image:` service is not exempt.
/// A `podman tag` that moves the name to a different image leaves the config
/// hash alone and must still recreate, which is what `up --pull always` on a
/// moved upstream tag looks like. Both images are built locally so the test
/// needs nothing but `alpine:latest`.
#[tokio::test]
async fn a_retagged_image_recreates_an_image_service() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("rci-retag");
	let tag = format!("localhost/{proj}-pinned:latest");
	let build = |contents: &[u8]| {
		fs::write(dir.path().join("Dockerfile"), contents).unwrap();
		let out = std::process::Command::new("podman")
			.args(["build", "-q", "-t", &tag, "-f", "Dockerfile", "."])
			.current_dir(dir.path())
			.output()
			.expect("podman build");
		assert!(
			out.status.success(),
			"podman build failed: {}",
			String::from_utf8_lossy(&out.stderr)
		);
	};
	build(V1);
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let yaml =
		format!("services:\n  web:\n    image: {tag}\n    command: [\"sleep\", \"infinity\"]\n");
	let file = parse_str(&yaml).unwrap();
	let cname = format!("{proj}-web-1");

	engine.up(&file).await.unwrap();
	engine
		.test_exec_capture(&cname, write_marker())
		.await
		.unwrap();
	// Same tag, different image underneath: the hash is identical, the ID is not.
	build(V2);
	engine.up(&file).await.unwrap();
	let version = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/version".into()])
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", &tag])
		.output();

	assert_eq!(
		version.trim(),
		"v2",
		"an image: service whose tag moved to a new image kept the old container"
	);
}
