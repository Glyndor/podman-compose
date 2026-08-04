//! A lifecycle POST whose response is dropped is resolved out of band (#1339).
//!
//! Measured on Podman 6 under concurrency: the drops land on state-changing
//! POSTs — `exec`, `restart`, `stop`, container `DELETE` — and follow a slow
//! one, a restart that burned its full stop grace before the next request lost
//! its response. It is not a client deadline (`READ_TIMEOUT` is 120s) and not a
//! pooled-connection race (there is no pool; every request opens a fresh
//! socket), so the transport genuinely cannot say whether the operation ran.
//!
//! These pin the three answers: the container reached the goal, it did not, and
//! the re-check itself could not be read. Only the first is success.

use super::commands::LifecycleGoal;

#[test]
fn a_goal_is_reached_only_by_the_state_that_satisfies_it() {
	assert!(LifecycleGoal::Running.reached(Some("running")));
	assert!(!LifecycleGoal::Running.reached(Some("exited")));
	assert!(!LifecycleGoal::Running.reached(Some("paused")));
	// A container that no longer exists never satisfies `Running`, and always
	// satisfies `NotRunning` — `rm` and a lost `kill` response both land here.
	assert!(!LifecycleGoal::Running.reached(None));
	assert!(LifecycleGoal::NotRunning.reached(None));
	assert!(LifecycleGoal::NotRunning.reached(Some("exited")));
	assert!(!LifecycleGoal::NotRunning.reached(Some("running")));
}

#[cfg(unix)]
mod over_the_wire {
	use super::super::commands::LifecycleGoal;
	use crate::engine::fake_podman::{self, FakeReply};
	use crate::engine::Engine;
	use crate::libpod::API_PREFIX;

	fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
		Engine::with_base_dir(client, project.into(), std::env::temp_dir())
	}

	/// Drop the response to the lifecycle POST, and answer the state re-check
	/// with `state`. `None` reports the container as absent.
	fn fake_dropping_the_op(state: Option<&'static str>) -> fake_podman::FakePodman {
		fake_podman::start_replying(move |method, target| {
			if method == "POST" && target.contains("/proj-web-1/") {
				// Accept, then hang up without a response — the one shape that
				// produces hyper's `IncompleteMessage`.
				return FakeReply::ClosedWithoutResponse;
			}
			if method == "GET" && target.contains("/containers/json") {
				let body = match state {
					Some(s) => format!(
						r#"[{{"Id":"abc","Names":["/proj-web-1"],"Image":"i","Status":"","State":"{s}"}}]"#
					),
					None => "[]".to_string(),
				};
				return FakeReply::Body(200, body);
			}
			FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
		})
	}

	fn start_path() -> String {
		format!("{API_PREFIX}/containers/proj-web-1/start")
	}

	#[tokio::test]
	async fn a_lost_response_succeeds_when_the_container_reached_the_goal() {
		let fake = fake_dropping_the_op(Some("running"));
		let engine = engine_with(fake.client(), "proj");
		let acted = engine
			.run_lifecycle_op(
				&start_path(),
				"proj-web-1",
				"Started",
				LifecycleGoal::Running,
			)
			.await
			.expect("the container is running, so the operation landed");
		assert!(acted, "a confirmed operation counts as having acted");
	}

	#[tokio::test]
	async fn a_lost_response_fails_when_the_container_did_not_reach_the_goal() {
		let fake = fake_dropping_the_op(Some("exited"));
		let engine = engine_with(fake.client(), "proj");
		engine
			.run_lifecycle_op(
				&start_path(),
				"proj-web-1",
				"Started",
				LifecycleGoal::Running,
			)
			.await
			.expect_err("the container is not running, so nothing confirms the start");
	}

	/// Fail closed. An unreadable re-check is not confirmation, and reporting
	/// success without one is the defect this whole path exists to prevent.
	#[tokio::test]
	async fn a_lost_response_fails_when_the_state_cannot_be_re_checked() {
		let fake = fake_podman::start_replying(|method, target| {
			if method == "POST" && target.contains("/proj-web-1/") {
				return FakeReply::ClosedWithoutResponse;
			}
			// The re-check itself errors.
			FakeReply::Body(500, r#"{"message":"boom"}"#.to_string())
		});
		let engine = engine_with(fake.client(), "proj");
		engine
			.run_lifecycle_op(
				&start_path(),
				"proj-web-1",
				"Started",
				LifecycleGoal::Running,
			)
			.await
			.expect_err("an unreadable re-check must not be read as success");
	}

	/// A gone container satisfies `NotRunning`, which is what a lost `kill`
	/// response looks like when the kill actually worked.
	#[tokio::test]
	async fn a_lost_kill_response_succeeds_when_the_container_is_gone() {
		let fake = fake_dropping_the_op(None);
		let engine = engine_with(fake.client(), "proj");
		let acted = engine
			.run_lifecycle_op(
				&format!("{API_PREFIX}/containers/proj-web-1/kill?signal=SIGKILL"),
				"proj-web-1",
				"Killed",
				LifecycleGoal::NotRunning,
			)
			.await
			.expect("the container is gone, so the kill landed");
		assert!(acted);
	}
}
