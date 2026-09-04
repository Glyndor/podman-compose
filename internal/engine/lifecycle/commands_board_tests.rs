//! The board the level-walking lifecycle commands open.
//!
//! Every one of `restart`, `stop`, `start`, `kill`, `pause`, `unpause` and `rm`
//! used to print plain append lines with no spinner, no elapsed column and no
//! marker, so the feel changed from one command to the next (#1671). What is
//! asserted here is the part a unit test can see: the board is opened over the
//! containers Podman actually has, in the order the command walks them, and it
//! is closed again on the way out.

use crate::engine::fake_podman;
use crate::engine::Engine;
use crate::ui::progress::capture::Capture;
use crate::ui::progress::Kind;

/// Two services, `two` after `one`, so the level order is `one` then `two` and
/// the reversed order (`stop`, `rm`) is `two` then `one`.
const FILE: &str = "\
services:
  one:
    image: alpine
  two:
    image: alpine
    depends_on:
      - one
";

/// The project listing every command below prefetches: one live replica per
/// service, handed back in the shuffled order a real libpod is free to use.
const LIVE: &str = r#"[
	{"Names":["/proj-two-1"],"State":"running","Labels":{"podup.service":"two"}},
	{"Names":["/proj-one-1"],"State":"running","Labels":{"podup.service":"one"}}
]"#;

/// A fake that answers the project listing and accepts every state change.
fn engine() -> (fake_podman::FakePodman, Engine) {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, LIVE.to_string())
		} else {
			(200, String::new())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	(fake, engine)
}

fn file() -> crate::compose::types::ComposeFile {
	crate::parse_str(FILE).expect("the fixture parses")
}

/// Every row is a container, which is what these commands act on.
fn all_containers(rows: &[(Kind, String)]) -> bool {
	rows.iter().all(|(kind, _)| *kind == Kind::Container)
}

#[tokio::test]
async fn restart_stop_start_draw_the_board_for_every_container() {
	let (_fake, engine) = engine();
	let file = file();

	let capture = Capture::start();
	engine
		.restart_with_options(&file, &[], false)
		.await
		.expect("restart against the fake succeeds");
	assert!(all_containers(&capture.rows()), "{:?}", capture.rows());
	assert_eq!(
		capture.names(),
		vec!["proj-one-1", "proj-two-1"],
		"restart walks the dependency order, so the board is seeded in it"
	);
	assert!(capture.every_board_ended(), "the board must be closed");
	drop(capture);

	let capture = Capture::start();
	engine.stop(&file, &[]).await.expect("stop succeeds");
	assert_eq!(
		capture.names(),
		vec!["proj-two-1", "proj-one-1"],
		"stop inverts the levels, and the board is seeded in the inverted order"
	);
	assert!(capture.every_board_ended());
	drop(capture);

	let capture = Capture::start();
	engine.start(&file, &[]).await.expect("start succeeds");
	assert_eq!(capture.names(), vec!["proj-one-1", "proj-two-1"]);
	assert!(capture.every_board_ended());
}

#[tokio::test]
async fn kill_pause_unpause_rm_draw_the_board_for_every_container() {
	let (_fake, engine) = engine();
	let file = file();

	let capture = Capture::start();
	engine
		.kill(&file, &[], "SIGTERM")
		.await
		.expect("kill succeeds");
	assert_eq!(capture.names(), vec!["proj-one-1", "proj-two-1"]);
	assert!(capture.every_board_ended());
	drop(capture);

	let capture = Capture::start();
	engine.pause(&file, &[]).await.expect("pause succeeds");
	assert_eq!(capture.names(), vec!["proj-one-1", "proj-two-1"]);
	assert!(capture.every_board_ended());
	drop(capture);

	let capture = Capture::start();
	engine.unpause(&file, &[]).await.expect("unpause succeeds");
	assert_eq!(capture.names(), vec!["proj-one-1", "proj-two-1"]);
	assert!(capture.every_board_ended());
	drop(capture);

	let capture = Capture::start();
	engine
		.rm_with_options(&file, &[], true, false)
		.await
		.expect("rm succeeds");
	assert!(all_containers(&capture.rows()));
	assert_eq!(
		capture.names(),
		vec!["proj-two-1", "proj-one-1"],
		"rm inverts the levels like stop does"
	);
	assert!(capture.every_board_ended());
	let verbs: Vec<(String, String)> = capture
		.verbs()
		.into_iter()
		.map(|(_, name, verb)| (name, verb))
		.collect();
	for name in ["proj-two-1", "proj-one-1"] {
		let opened = verbs
			.iter()
			.position(|(n, v)| n == name && v == "Removing")
			.unwrap_or_else(|| panic!("{name} opens with Removing: {verbs:?}"));
		let closed = verbs
			.iter()
			.position(|(n, v)| n == name && v == "Removed")
			.unwrap_or_else(|| panic!("{name} closes with Removed: {verbs:?}"));
		assert!(
			opened < closed,
			"Removing precedes Removed for {name}, so the row has a start time (#1686)"
		);
	}
}

/// A service the file defines but that Podman has never created contributes no
/// row: a row that never moves reads as something hung, which is the rule
/// `down_resources` already follows.
#[tokio::test]
async fn a_never_created_service_gets_no_row() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(
				200,
				r#"[{"Names":["/proj-one-1"],"Labels":{"podup.service":"one"}}]"#.to_string(),
			)
		} else {
			(200, String::new())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	let capture = Capture::start();
	engine.stop(&file(), &[]).await.expect("stop succeeds");
	assert_eq!(capture.names(), vec!["proj-one-1"]);
}

/// Every final verb has a working verb the row shows while the engine acts,
/// so a ten-second stop is a turning spinner and a clock, not a frozen
/// `Pending`.
#[test]
fn every_final_verb_has_a_working_verb() {
	for (done, doing) in [
		("Started", "Starting"),
		("Stopped", "Stopping"),
		("Restarted", "Restarting"),
		("Killed", "Killing"),
		("Paused", "Pausing"),
		("Unpaused", "Unpausing"),
		("Removed", "Removing"),
	] {
		assert_eq!(super::working_verb(done), doing);
	}
}

/// `rm --stop` resumes only what is paused before it stops (#1688). With
/// nothing paused it draws no board and sends no unpause request; with one
/// paused container it draws a board for that one alone.
#[tokio::test]
async fn unpause_paused_touches_only_the_paused_containers() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(
				200,
				r#"[{"Names":["/proj-one-1"],"State":"running","Labels":{"podup.service":"one"}},
				    {"Names":["/proj-two-1"],"State":"running","Labels":{"podup.service":"two"}}]"#
					.to_string(),
			)
		} else {
			(200, String::new())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	let capture = Capture::start();
	engine
		.unpause_paused(&file(), &[])
		.await
		.expect("nothing paused is not an error");
	assert!(
		capture.boards().is_empty(),
		"nothing paused draws no board: {:?}",
		capture.boards()
	);
	{
		let seen = fake.requests.lock().unwrap();
		assert!(
			!seen.iter().any(|r| r.contains("/unpause")),
			"nothing paused sends no unpause request: {seen:?}"
		);
	}
	drop(capture);

	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(
				200,
				r#"[{"Names":["/proj-one-1"],"State":"running","Labels":{"podup.service":"one"}},
				    {"Names":["/proj-two-1"],"State":"paused","Labels":{"podup.service":"two"}}]"#
					.to_string(),
			)
		} else {
			(200, String::new())
		}
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	let capture = Capture::start();
	engine
		.unpause_paused(&file(), &[])
		.await
		.expect("the paused one is resumed");
	assert_eq!(
		capture.names(),
		vec!["proj-two-1"],
		"only the paused container gets a row"
	);
	assert!(
		capture
			.verbs()
			.iter()
			.any(|(_, n, v)| n == "proj-two-1" && v == "Unpaused"),
		"and it closes Unpaused: {:?}",
		capture.verbs()
	);
	assert!(capture.every_board_ended());
}
