use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::ExecOptions;
use crate::compose::parse_str;
use crate::engine::fake_podman::{self, FakeReply};
use crate::engine::Engine;

/// Answer the exec create by dropping the connection the first `drops` times
/// and replying normally after that. Returns the fake and the call counter.
fn fake_dropping_creates(drops: usize) -> (fake_podman::FakePodman, Arc<AtomicUsize>) {
	let creates = Arc::new(AtomicUsize::new(0));
	let seen = creates.clone();
	let fake = fake_podman::start_replying(move |method, target| {
		if method == "POST" && target.ends_with("/exec") {
			let n = seen.fetch_add(1, Ordering::SeqCst);
			if n < drops {
				return FakeReply::ClosedWithoutResponse;
			}
			return FakeReply::Body(201, r#"{"Id":"exec-1"}"#.to_string());
		}
		if method == "POST" && target.contains("/exec/") {
			return FakeReply::Body(200, String::new());
		}
		if method == "GET" && target.contains("/containers/json") {
			return FakeReply::Body(
				200,
				r#"[{"Id":"c1","Names":["/proj-web-1"],"Image":"i","Status":"","State":"running"}]"#
					.to_string(),
			);
		}
		FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
	});
	(fake, creates)
}

/// Drive the real `exec` entry point, detached so the hijacked streaming path
/// is out of the picture: the retry under test is on the CREATE, which both
/// paths share. Driving `test_exec_capture` instead would have measured
/// nothing: it builds its own request and never reaches this code.
async fn run_exec(fake: &fake_podman::FakePodman) -> crate::error::Result<()> {
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());
	let file = parse_str("services:\n  web:\n    image: alpine:latest\n").unwrap();
	engine
		.exec_with_options(
			&file,
			"web",
			vec!["true".to_string()],
			ExecOptions::default()
				.with_no_tty_for_test(true)
				.with_detach_for_test(true),
		)
		.await
}

#[tokio::test]
async fn a_dropped_exec_create_is_retried_once_and_succeeds() {
	let (fake, creates) = fake_dropping_creates(1);
	run_exec(&fake).await.expect("the retry answers");
	assert_eq!(
		creates.load(Ordering::SeqCst),
		2,
		"the create must be attempted twice: once dropped, once retried"
	);
}

/// Once only. A second drop is a daemon problem, not a transient, and
/// retrying forever would turn a broken socket into a hang.
#[tokio::test]
async fn a_second_dropped_exec_create_is_not_retried_again() {
	let (fake, creates) = fake_dropping_creates(2);
	run_exec(&fake)
		.await
		.expect_err("two drops in a row is a failure, not something to keep retrying");
	assert_eq!(
		creates.load(Ordering::SeqCst),
		2,
		"exactly two attempts, never a third"
	);
}
