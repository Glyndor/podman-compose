use std::path::PathBuf;

use crate::compose::types::{ComposeFile, Service};
use crate::engine::fake_podman::{self, FakeReply};
use crate::engine::Engine;
use crate::error::ComposeError;

#[tokio::test]
async fn export_to_an_unwritable_destination_surfaces_iopath() {
	// A single-replica service so the container name resolves without
	// inspecting Podman.
	let mut file = ComposeFile::default();
	file.services.insert(
		"web".into(),
		Service {
			image: Some("nginx:1.27".into()),
			..Default::default()
		},
	);

	// The fake answers the streaming GET the same way it would for a
	// healthy container — podup never has to read a real byte from us,
	// because the destination file fails to open before the body is
	// consumed. The chunked body is what `get_stream` accepts. The
	// target carries the `http://localhost` prefix the client builds,
	// so the matcher looks for the trailing `/export` route instead.
	let fake = fake_podman::start_replying(|method, target| {
		if method == "GET" && target.contains("/export") {
			// One empty chunk is enough — the test errors before the
			// body is fully drained.
			return FakeReply::ChunkedEnd(vec![String::new()]);
		}
		FakeReply::Body(404, r#"{"message":"not used"}"#.to_string())
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());

	// A path under a directory that does not exist: `File::create`
	// fails synchronously at the parent-component lookup, before any
	// byte hits the (would-be) sink. The production code wraps that
	// io error in `IoPath` with the path; the test confirms both.
	let dest = PathBuf::from("/nonexistent-dir-7c4e1a/out.x.tar");

	let err = engine
		.export(&file, "web", Some(dest.clone()), None)
		.await
		.expect_err("an unwritable destination must surface as an error");

	let msg = err.to_string();
	let ComposeError::IoPath { path, .. } = err else {
		panic!("expected IoPath for an unwritable -o FILE, got: {msg:?}");
	};
	assert_eq!(
		path,
		dest.display().to_string(),
		"the destination path must appear in the error"
	);
	assert!(
		msg.contains(&dest.display().to_string()),
		"the rendered message must name the destination, got: {msg}"
	);
}
