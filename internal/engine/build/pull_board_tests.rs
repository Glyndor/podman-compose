//! The board a standalone `pull` opens.
//!
//! `pull` printed plain append lines with no spinner, no elapsed column and no
//! marker while `up` over the same images drew a board (#1671). What is
//! asserted here is what a unit test can see: the board is seeded with the
//! image set before the first `Pulling`, one row per distinct image, and it is
//! closed on the way out, including the way out of a failure.

use crate::engine::fake_podman::{self, FakeReply};
use crate::engine::Engine;
use crate::ui::progress::capture::Capture;
use crate::ui::progress::Kind;

/// Three services over two distinct images: the shared one is one row, because
/// the row is the image and not the service.
const FILE: &str = "\
services:
  one:
    image: img-a
  two:
    image: img-b
  three:
    image: img-a
";

/// A fake that accepts every pull and reports the image present afterwards, so
/// the pass succeeds.
fn engine() -> (fake_podman::FakePodman, Engine) {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "GET" && target.contains("/images/") {
			(200, "{}".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	(fake, engine)
}

/// A fake whose `/images/pull` stream emits the given JSON lines as a properly
/// terminated chunked body, the way a real libpod would. Each line is wrapped
/// in `{"stream":"…"}` because that is the wire shape of an `ImagePullProgress`
/// event.
fn engine_streaming(chunks: Vec<String>) -> (fake_podman::FakePodman, Engine) {
	let fake = fake_podman::start_replying(move |method, target| {
		if method == "POST" && target.contains("/images/pull") {
			FakeReply::ChunkedEnd(chunks.clone())
		} else if method == "GET" && target.contains("/images/") {
			FakeReply::Body(200, "{}".to_string())
		} else {
			FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	(fake, engine)
}

#[tokio::test]
async fn pull_draws_the_board_for_every_image() {
	let (_fake, engine) = engine();
	let file = crate::parse_str(FILE).expect("the fixture parses");

	let capture = Capture::start();
	engine
		.pull(&file)
		.await
		.expect("a pull the fake accepts succeeds");
	let mut names = capture.names();
	names.sort();
	assert_eq!(
		names,
		vec!["img-a", "img-b"],
		"one row per distinct image, not per service"
	);
	assert!(
		capture.rows().iter().all(|(kind, _)| *kind == Kind::Image),
		"every row is an image: {:?}",
		capture.rows()
	);
	assert!(
		capture.every_board_ended(),
		"the board must be closed, or a terminal is left without a cursor"
	);
}

/// A pull that fails still closes its board. The live region hides the cursor,
/// so an early return through an open one leaves the terminal without a caret.
#[tokio::test]
async fn a_failed_pull_still_closes_the_board() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(200, r#"{"error":"registry unreachable"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	let file = crate::parse_str("services:\n  one:\n    image: img-a\n").expect("parses");

	let capture = Capture::start();
	engine
		.pull(&file)
		.await
		.expect_err("an in-band pull error must fail the pass");
	assert_eq!(capture.names(), vec!["img-a"]);
	assert!(capture.every_board_ended());
}

/// Off a terminal, the board opens no live region: no cursor hiding, no
/// repaint, and every event goes out as the plain line it always was (the
/// plain sink writes it through `write_progress_line`, the one line format both
/// renderers share). `cargo test` redirects stderr, which is the same
/// condition a pipe puts podup in; the end-to-end version of this, with the
/// real binary and its stderr piped, is
/// `a_piped_pull_keeps_the_plain_lines` in `tests/reporting_contract.rs`.
#[tokio::test]
async fn a_pull_off_a_terminal_opens_no_live_region() {
	let (_fake, engine) = engine();
	let file = crate::parse_str(FILE).expect("the fixture parses");

	let capture = Capture::start();
	engine.pull(&file).await.expect("the pull succeeds");
	assert!(
		!capture.any_live(),
		"a redirected stderr must not get a repainted region"
	);
}

// #1674: while an image is pulling, the board row's verb carries the layer
// count. Three `Copying blob` lines, in arrival order, are the input the parser
// sees; the test asserts the verbs it produces, which is the row's text on a
// terminal. Kept here so the parser stays testable without a tty, which is the
// only place the assertion can be checked against a number.
#[test]
fn a_pull_row_counts_copied_layers() {
	let mut progress = super::PullStreamProgress::new();
	let verbs: Vec<String> = [
		"Copying blob sha256:aaa",
		"Copying blob sha256:bbb",
		"Copying blob sha256:ccc",
	]
	.iter()
	.filter_map(|line| progress.observe(line))
	.collect();
	assert_eq!(
		verbs,
		vec![
			"Pulling 1 layer".to_string(),
			"Pulling 2 layers".to_string(),
			"Pulling 3 layers".to_string(),
		],
		"before the manifest is read, the verb counts the layers seen so far",
	);
	// Once `Copying config` arrives the verb switches to the `done/total`
	// form, which is what the row settles on for the rest of the pull.
	assert_eq!(
		progress.observe("Copying config sha256:cfg"),
		Some("Pulling 3/3".to_string()),
		"the manifest line flips the row from `N layers` to `done/total`",
	);
	assert_eq!(
		progress.observe("Copying blob sha256:ddd"),
		Some("Pulling 4/4".to_string()),
		"subsequent `Copying blob` lines use the settled total",
	);
}

/// A `Copying blob <digest>` line that libpod repeats on a retry must not move
/// the total: the distinct-digest set already has the digest. The done count
/// (raw line count) does grow, so under a retried layer the verb reads
/// `Pulling 2/1` for that one step. The cost of accepting a raw done counter;
/// the alternative, a separate "in-flight" counter we have no signal for,
/// would be guessing (#1674).
#[test]
fn a_retried_blob_is_counted_once() {
	let mut progress = super::PullStreamProgress::new();
	let verbs: Vec<String> = [
		"Copying blob sha256:aaa",
		"Copying blob sha256:aaa", // retry of the same digest
		"Copying blob sha256:bbb",
	]
	.iter()
	.filter_map(|line| progress.observe(line))
	.collect();
	assert_eq!(
		verbs,
		vec![
			"Pulling 1 layer".to_string(),  // aaa seen
			"Pulling 1 layer".to_string(),  // aaa again, set already had it
			"Pulling 2 layers".to_string(), // bbb now in the set
		],
		"the retried blob does not grow the distinct-digest set",
	);
}

/// A piped pull must print `Pulling` and `Pulled` and nothing in between.
///
/// This is the half of #1674 the contract tests cannot see without a live
/// terminal: each `Copying blob` line *would* push a new verb onto the row on a
/// tty, but the plain sink would write one line per call, polluting a CI log.
/// The engine gates the per-blob `start` calls on the same predicate that
/// opens the live region, so under a redirected stderr (the `cargo test`
/// condition, and the condition any pipe puts podup in) the row is seeded with
/// `Pulling`, the stream is read without row transitions, and `Pulled` closes
/// the row.
///
/// Driving this through the fake Podman (and not by mocking the parser) is
/// what pins the gate in pull.rs and not somewhere inside
/// `internal/ui/progress/`, which is where the contract belongs.
#[tokio::test]
async fn a_piped_pull_prints_only_pulling_and_pulled() {
	// Each chunk is the wire form of one `ImagePullProgress` event, which is a
	// JSON object terminated by `\n`. The newline is what `parse_json_lines`
	// splits on; without it the parser would buffer every chunk into one giant
	// record at end of stream and try (and fail) to parse five objects as one
	// (#1104 covers the shape). Chunked transfer decoding (handled by hyper)
	// strips the `<hex>\r\n` framing around each chunk, so the body bytes the
	// parser sees are exactly the chunk strings concatenated; each must end in
	// `\n` so the parser can find its line boundary.
	let chunks = vec![
		"{\"stream\":\"Copying blob sha256:aaa\"}\n".to_string(),
		"{\"stream\":\"Copying blob sha256:bbb\"}\n".to_string(),
		"{\"stream\":\"Copying blob sha256:ccc\"}\n".to_string(),
		"{\"stream\":\"Copying config sha256:cfg\"}\n".to_string(),
		"{\"stream\":\"Copying blob sha256:ddd\"}\n".to_string(),
	];
	let (_fake, engine) = engine_streaming(chunks);
	let file = crate::parse_str("services:\n  one:\n    image: img-a\n").expect("parses");

	let capture = Capture::start();
	engine
		.pull(&file)
		.await
		.expect("a streaming pull the fake accepts succeeds");

	let verbs: Vec<String> = capture
		.verbs()
		.into_iter()
		.filter(|(kind, _, _)| *kind == Kind::Image)
		.map(|(_, _, verb)| verb)
		.collect();
	assert_eq!(
		verbs,
		vec!["Pulling", "Pulled"],
		"a piped pull shows only the start and end verbs; \
		 the per-blob verbs would write one line per blob to the log: {verbs:?}",
	);
	assert!(
		capture.every_board_ended(),
		"the board still closes on the way out, even with no intermediate starts",
	);
}
