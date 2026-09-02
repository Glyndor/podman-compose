//! A replaced container says so.
//!
//! Recreating a container destroys its writable layer. Until #1619 the
//! progress stream reported that with the same `Starting`/`Started` pair it
//! uses for a container that did not exist, so the only way to learn that
//! anything had been removed was to compare container IDs by hand, and
//! `--force-recreate` / `--no-recreate` could not be told apart from a plain
//! `up` in the output. These tests pin the vocabulary per outcome, on a real
//! Podman, by reading the stderr stream the way a CI log would.
//!
//! Each assertion names the container, not just the verb: a looser check
//! passed once with the container's own event deleted because the network's
//! `Creating` satisfied it (see `reporting_contract.rs`).

mod harness;

use harness::{podman_up, Project};
use std::fs;
use tempfile::tempdir;

fn compose(command: &str) -> String {
	format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"{command}\"]\n"
	)
}

fn project(tag: &str) -> Project {
	let dir = tempdir().unwrap();
	let path = dir.path().join("compose.yaml");
	fs::write(&path, compose("infinity")).unwrap();
	Project {
		compose: path.to_string_lossy().into_owned(),
		name: format!("t{}-{tag}", std::process::id()),
		_dir: dir,
	}
}

/// `true` when some line of `stderr` names both the container and one of the
/// verbs. Per resource, deliberately.
fn says(stderr: &str, container: &str, verbs: &[&str]) -> bool {
	stderr
		.lines()
		.any(|l| l.contains(container) && verbs.iter().any(|v| l.contains(v)))
}

/// A container that did not exist is `Starting`/`Started`, never `Recreating`.
/// This is the control for the tests below: without it a stream that said
/// `Recreating` on every run would pass them all.
#[tokio::test]
async fn a_first_up_creates_and_never_says_recreating() {
	if !podman_up().await {
		return;
	}
	let p = project("rcv-first");
	let container = format!("{}-web-1", p.name);
	let first = p.progress(&["up", "-d"]);
	assert!(
		says(&first, &container, &["Starting"]) && says(&first, &container, &["Started"]),
		"a new container is Starting/Started; got:\n{first}"
	);
	assert!(
		!first.contains("Recreat"),
		"nothing existed, so nothing can have been recreated; got:\n{first}"
	);
}

/// An unchanged `up` leaves the container alone and says `Running`, so the
/// skip path and the recreate path cannot share a word either.
#[tokio::test]
async fn an_unchanged_up_says_running_not_recreating() {
	if !podman_up().await {
		return;
	}
	let p = project("rcv-same");
	let container = format!("{}-web-1", p.name);
	p.progress(&["up", "-d"]);
	let again = p.progress(&["up", "-d"]);
	assert!(
		says(&again, &container, &["Running"]),
		"an unchanged container reports Running; got:\n{again}"
	);
	assert!(
		!again.contains("Recreat") && !says(&again, &container, &["Starting", "Started"]),
		"an unchanged container was reported as replaced or created; got:\n{again}"
	);
}

/// A changed config replaces the container, and the stream says so with a
/// word that only the recreate branch emits.
#[tokio::test]
async fn a_changed_config_says_recreating_and_recreated() {
	if !podman_up().await {
		return;
	}
	let p = project("rcv-change");
	let container = format!("{}-web-1", p.name);
	p.progress(&["up", "-d"]);
	fs::write(&p.compose, compose("120")).unwrap();
	let changed = p.progress(&["up", "-d"]);
	assert!(
		says(&changed, &container, &["Recreating"]),
		"a replaced container needs its own transition; got:\n{changed}"
	);
	assert!(
		says(&changed, &container, &["Recreated"]),
		"and its own ending; got:\n{changed}"
	);
	assert!(
		!says(&changed, &container, &["Starting", "Started"]),
		"Starting/Started is the word for a container that did not exist; got:\n{changed}"
	);
}

/// `--force-recreate` is verifiable from the output: it replaces a container
/// whose config did not change, and the stream says `Recreating`, not
/// `Running`. This is the case a `present` set fetched only on the
/// non-forced path would get wrong.
#[tokio::test]
async fn force_recreate_says_recreating_on_an_unchanged_config() {
	if !podman_up().await {
		return;
	}
	let p = project("rcv-force");
	let container = format!("{}-web-1", p.name);
	p.progress(&["up", "-d"]);
	let forced = p.progress(&["up", "-d", "--force-recreate"]);
	assert!(
		says(&forced, &container, &["Recreating"]) && says(&forced, &container, &["Recreated"]),
		"--force-recreate replaced the container and must say so; got:\n{forced}"
	);
	assert!(
		!says(&forced, &container, &["Running", "Starting", "Started"]),
		"a forced recreate must not read as a skip or a first start; got:\n{forced}"
	);
}

/// `create` over an existing container replaces it too, and the stopped
/// outcome keeps the same pair: the word is about what happened to the old
/// container, not about whether the new one was started.
#[tokio::test]
async fn create_over_an_existing_container_says_recreating() {
	if !podman_up().await {
		return;
	}
	let p = project("rcv-create");
	let container = format!("{}-web-1", p.name);
	let first = p.progress(&["create"]);
	assert!(
		says(&first, &container, &["Creating"]) && says(&first, &container, &["Created"]),
		"a first create is Creating/Created; got:\n{first}"
	);
	fs::write(&p.compose, compose("120")).unwrap();
	let again = p.progress(&["create"]);
	assert!(
		says(&again, &container, &["Recreating"]) && says(&again, &container, &["Recreated"]),
		"create over a changed container replaces it and must say so; got:\n{again}"
	);
}
