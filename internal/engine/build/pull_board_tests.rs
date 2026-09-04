//! The board a standalone `pull` opens.
//!
//! `pull` printed plain append lines with no spinner, no elapsed column and no
//! marker while `up` over the same images drew a board (#1671). What is
//! asserted here is what a unit test can see: the board is seeded with the
//! image set before the first `Pulling`, one row per distinct image, and it is
//! closed on the way out, including the way out of a failure.

use crate::engine::fake_podman;
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
