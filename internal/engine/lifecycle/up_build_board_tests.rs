//! `up --build` draws a single board, with the image row at the top and the
//! network/container rows that follow.
//!
//! Two assertions, kept on the same file because they pin the two halves of
//! the `#1700` contract:
//!
//! - `up_with_a_missing_image_builds_on_the_same_board` (in `images_tests.rs`)
//!   pins the *missing-image* path: an `up` without `--build` whose image is
//!   not local builds it on the `up` board, with the image row above the
//!   service's container row. That path is the one `#1681` kept on purpose.
//!
//! - `up_build_draws_a_single_board_with_the_image_row_first` (here) pins the
//!   `--build` path: a forced rebuild runs in the *same* board the rest of
//!   `up` is drawing on, with the image row first and the build verb closing
//!   before any container row moves. Before this fix the build drew its own
//!   board first and `up` opened a second one.

use crate::engine::fake_podman::{self, FakeReply};
use crate::engine::Engine;
use crate::ui::progress::capture::Capture;
use crate::ui::progress::Kind;

const STREAM: &[&str] = &[
	"{\"stream\":\"STEP 1/2: FROM docker.io/library/alpine:3.20\\n\"}\n",
	"{\"stream\":\"--> 3f3c8b769775\\n\"}\n",
	"{\"stream\":\"STEP 2/2: CMD [\\\"echo\\\",\\\"hi\\\"]\\n\"}\n",
	"{\"stream\":\"--> 9f3c8b769775\\n\"}\n",
	"{\"stream\":\"COMMIT localhost/ux-up-build:1\\n\"}\n",
	"{\"stream\":\"--> sha256:1111111111111111111111111111111111111111111111111111111111111111\\n\"}\n",
	"{\"stream\":\"Successfully tagged localhost/ux-up-build:1\\n\"}\n",
];

/// `up -d --build` on a two-service project where exactly one service has a
/// `build:` block draws one board: the image row first, then the network and
/// container rows, and the image row's `Built` verb closes before any
/// container row moves. The build verb itself is the same `Building`/`Built`
/// pair the standalone `build` produces. Only the surrounding board is the
/// one `up` already opened.
#[tokio::test]
async fn up_build_draws_a_single_board_with_the_image_row_first() {
	let chunks: Vec<String> = STREAM.iter().map(|s| s.to_string()).collect();
	let fake = fake_podman::start_replying(move |method, target| {
		if method == "POST" && target.contains("/build?") {
			FakeReply::ChunkedEnd(chunks.clone())
		} else if method == "POST" && target.contains("/images/") && target.contains("/tag") {
			FakeReply::Body(200, String::new())
		} else if method == "POST" && target.contains("/images/pull") {
			// The sidecar's image has to be pullable for `start_services_by_dependency`
			// to land its create/start path. The build service's image is absent
			// before its build runs, so its inspect returns 404 below.
			FakeReply::Body(200, String::new())
		} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
			if target.contains("ux-up-build") {
				// The build service's image is absent before its build runs.
				FakeReply::Body(404, r#"{"message":"no such image"}"#.to_string())
			} else {
				// The sidecar's image is present locally.
				FakeReply::Body(200, r#"{"Id":"sha256:cafe"}"#.to_string())
			}
		} else if method == "GET" && target.contains("/containers/json") {
			FakeReply::Body(200, "[]".to_string())
		} else if method == "POST" && target.contains("/containers/create") {
			FakeReply::Body(200, "{}".to_string())
		} else if method == "POST" && target.contains("/start") {
			FakeReply::Body(200, String::new())
		} else {
			FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
		}
	});

	let ctx = tempfile::tempdir().expect("tempdir");
	std::fs::write(
		ctx.path().join("Dockerfile"),
		b"FROM docker.io/library/alpine:3.20\nCMD [\"echo\",\"hi\"]\n",
	)
	.expect("write Dockerfile");
	let mut engine = Engine::with_base_dir(fake.client(), "proj".into(), ctx.path().to_path_buf());
	engine.no_warn = true;

	let compose = crate::parse_str(
		"\
services:
  app:
    image: localhost/ux-up-build:1
    build:
      context: .
    command: [\"echo\",\"hi\"]
  sidecar:
    image: docker.io/library/alpine:3.20
    command: [\"echo\",\"hi\"]
",
	)
	.unwrap();

	let capture = Capture::start();
	engine
		.up_with_options(&compose, true, &[], &[], false, false, false, true)
		.await
		.expect("an up --build that builds its image succeeds");

	// One board: the seed before this fix was a separate `build_all` board
	// that opened and closed before `up` opened its own.
	let boards = capture.boards();
	assert_eq!(
		boards.len(),
		1,
		"`up --build` must draw a single board: {boards:?}"
	);

	// The image row sits before the network and container rows the rest of
	// `up` is responsible for. Two services, one with a build: the build
	// service's image is on top, the sidecar's container row follows.
	let names = capture.names();
	assert!(
		names.first().map(String::as_str) == Some("localhost/ux-up-build:1"),
		"the image row is first: {names:?}"
	);
	assert!(
		names.iter().any(|n| n == "proj-app-1"),
		"the build service's container row is on the same board: {names:?}"
	);
	assert!(
		names.iter().any(|n| n == "proj-sidecar-1"),
		"the sidecar's container row is on the same board: {names:?}"
	);

	// Build verb lifecycle on the same board: `Building` then `Built`, and
	// `Built` lands before any container row's first verb. Captured from
	// the per-thread event log so the test does not depend on whether the
	// engine opened a live region.
	let verbs = capture.verbs();
	let built_pos = verbs
		.iter()
		.position(|(_, n, v)| n == "localhost/ux-up-build:1" && v == "Built")
		.expect("the image row closes Built");
	let first_container_pos = verbs
		.iter()
		.position(|(_, n, _)| n == "proj-app-1" || n == "proj-sidecar-1")
		.expect("at least one container row receives an event");
	assert!(
		verbs
			.iter()
			.any(|(_, n, v)| n == "localhost/ux-up-build:1" && v == "Building"),
		"the image row opens Building: {verbs:?}"
	);
	assert!(
		built_pos < first_container_pos,
		"the image row's Built verb lands before any container row's first event: {verbs:?}"
	);

	// All rows are the kinds the board is responsible for: one Image row at
	// the top, plus Networks and Containers below. A wrong kind here would
	// mean a row got seeded against the wrong column.
	let rows = capture.rows();
	assert!(
		rows.iter().any(|(kind, _)| *kind == Kind::Image),
		"an Image row sits on the board: {rows:?}"
	);
	assert!(
		rows.iter()
			.all(|(kind, _)| matches!(kind, Kind::Image | Kind::Network | Kind::Container)),
		"every row is an Image, Network or Container: {rows:?}"
	);

	assert!(
		capture.every_board_ended(),
		"the single board closes on the way out"
	);
}
