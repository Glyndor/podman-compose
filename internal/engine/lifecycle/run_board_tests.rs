//! The board a one-off `run` opens over what it does before the container's
//! own output takes over.
//!
//! `run` printed plain append lines for the networks it ensures while `up` over
//! the same networks drew a board (#1671). The image row joins them when the
//! image still has to be acquired; an image already in local storage produces
//! no verb on this path, so seeding a row for it would leave a line reading
//! `Pending` behind every successful run.

use crate::engine::fake_podman;
use crate::engine::Engine;
use crate::ui::progress::capture::Capture;
use crate::ui::progress::Kind;

const FILE: &str = "\
services:
  app:
    image: img
networks:
  extra:
";

fn options() -> crate::engine::RunOptions {
	crate::engine::RunOptions {
		cmd: vec![],
		rm: false,
		// Detached, so the run returns as soon as the container is started and
		// the test never has to model a log stream.
		detach: true,
		env_overrides: vec![],
		name_override: None,
		service_ports: false,
	}
}

/// A fake with no image in local storage, no network yet, and a container
/// create/start that succeeds.
fn engine(image_present: bool) -> (fake_podman::FakePodman, Engine) {
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/images/") {
			if image_present {
				(200, "{}".to_string())
			} else {
				(404, r#"{"message":"no such image"}"#.to_string())
			}
		} else if method == "POST" {
			(200, r#"{"Id":"cafe"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	(fake, engine)
}

#[tokio::test]
async fn run_draws_the_board_for_its_network_and_image_rows() {
	let (_fake, engine) = engine(false);
	let file = crate::parse_str(FILE).expect("the fixture parses");

	let capture = Capture::start();
	engine
		.run(&file, "app", options())
		.await
		.expect("a detached run against the fake succeeds");
	let rows = capture.rows();
	assert!(
		rows.iter()
			.any(|(kind, name)| *kind == Kind::Network && name == "proj_extra"),
		"the project network is a row: {rows:?}"
	);
	assert_eq!(
		rows.last(),
		Some(&(Kind::Image, "img".to_string())),
		"the image the run has to acquire is the last row before the container: {rows:?}"
	);
	assert!(
		capture.every_board_ended(),
		"the board closes before the container's own output starts"
	);
}

/// An image already in local storage gets no row, because nothing on this path
/// reports it and a row that never moves reads as something hung.
#[tokio::test]
async fn a_present_image_gets_no_row() {
	let (_fake, engine) = engine(true);
	let file = crate::parse_str(FILE).expect("the fixture parses");

	let capture = Capture::start();
	engine
		.run(&file, "app", options())
		.await
		.expect("a detached run against the fake succeeds");
	assert!(
		!capture.rows().iter().any(|(kind, _)| *kind == Kind::Image),
		"{:?}",
		capture.rows()
	);
}
