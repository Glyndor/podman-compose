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

use super::drop_recheck::LifecycleGoal;

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
	use super::super::drop_recheck::LifecycleGoal;
	use crate::engine::fake_podman::{self, FakeReply};
	use crate::engine::Engine;
	use crate::libpod::API_PREFIX;

	pub(super) fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
		Engine::with_base_dir(client, project.into(), std::env::temp_dir())
	}

	/// Drop the response to the lifecycle POST, and answer the state re-check
	/// with `state`. `None` reports the container as absent.
	pub(super) fn fake_dropping_the_op(state: Option<&'static str>) -> fake_podman::FakePodman {
		fake_podman::start_replying(move |method, target| {
			// Both shapes the drops were measured on: the state-changing POSTs
			// and the container DELETE. Dropping only the POSTs let a removal
			// fall through to the 404 arm and be read as an idempotent no-op,
			// which is a test that measures nothing.
			let touches_it = target.contains("/proj-web-1");
			if touches_it && (method == "POST" || method == "DELETE") {
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
		let err = engine
			.run_lifecycle_op(
				&start_path(),
				"proj-web-1",
				"Started",
				LifecycleGoal::Running,
			)
			.await
			.expect_err("the container is not running, so nothing confirms the start");
		// Pin the variant: a dropped response that re-checks to a non-running
		// state must surface as the underlying libpod error, not as some
		// other variant a future refactor might swap in. `confirm_lost_response`
		// is the one place that does this — a regression here would otherwise
		// be invisible because the test would still see *some* Err.
		assert!(
			matches!(err, crate::error::ComposeError::Podman(_)),
			"expected ComposeError::Podman, got: {err:?}"
		);
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
		let err = engine
			.run_lifecycle_op(
				&start_path(),
				"proj-web-1",
				"Started",
				LifecycleGoal::Running,
			)
			.await
			.expect_err("an unreadable re-check must not be read as success");
		// Same shape as the test above: fail-closed is the contract, and the
		// specific variant is what the CLI's exit-code map keys on.
		assert!(
			matches!(err, crate::error::ComposeError::Podman(_)),
			"expected ComposeError::Podman, got: {err:?}"
		);
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

/// `Gone` is not `NotRunning`. A removal that lost its response must not be read
/// as success just because the container stopped — it has to be absent.
#[test]
fn gone_needs_absence_not_merely_a_stopped_container() {
	assert!(LifecycleGoal::Gone.reached(None));
	assert!(!LifecycleGoal::Gone.reached(Some("exited")));
	assert!(!LifecycleGoal::Gone.reached(Some("running")));
	// The distinction that matters: `exited` satisfies NotRunning and not Gone.
	assert!(LifecycleGoal::NotRunning.reached(Some("exited")));
}

#[cfg(unix)]
mod stop_and_remove {
	use super::over_the_wire::{engine_with, fake_dropping_the_op};

	/// `stop` does not go through `run_lifecycle_op`, so it carries the re-check
	/// itself. A container that is no longer running confirms the stop landed.
	#[tokio::test]
	async fn a_lost_stop_response_succeeds_when_the_container_is_not_running() {
		let fake = fake_dropping_the_op(Some("exited"));
		let engine = engine_with(fake.client(), "proj");
		engine
			.stop_container("proj-web-1", 10)
			.await
			.expect("the container is not running, so the stop landed");
	}

	#[tokio::test]
	async fn a_lost_stop_response_fails_while_the_container_still_runs() {
		let fake = fake_dropping_the_op(Some("running"));
		let engine = engine_with(fake.client(), "proj");
		let err = engine
			.stop_container("proj-web-1", 10)
			.await
			.expect_err("still running is not a stop");
		// The dropped-stop branch in `stop_container` falls through to
		// `confirm_lost_response` with `LifecycleGoal::NotRunning`, which
		// builds `ComposeError::Podman` on a state mismatch — pin the
		// variant so a regression that swaps in `StreamTruncated` (the
		// other "ended unexpectedly" variant) cannot silently satisfy this
		// test.
		assert!(
			matches!(err, crate::error::ComposeError::Podman(_)),
			"expected ComposeError::Podman, got: {err:?}"
		);
	}

	/// And removal, which needs the container **absent** — a stopped-but-present
	/// container would satisfy `NotRunning` and read a failed removal as success.
	#[tokio::test]
	async fn a_lost_removal_response_fails_when_the_container_is_merely_stopped() {
		let fake = fake_dropping_the_op(Some("exited"));
		let engine = engine_with(fake.client(), "proj");
		let err = engine
			.teardown_one_container("proj-web-1", 10, &[], false)
			.await
			.expect_err("a container that is still there was not removed");
		// Same pattern as the stop test: `teardown_one_container`'s dropped
		// arm goes through `confirm_lost_response` with `LifecycleGoal::Gone`,
		// and a stopped-but-present container fails `Gone`. The variant is
		// the contract — anything else would be a regression in the
		// fail-closed shape this whole file pins.
		assert!(
			matches!(err, crate::error::ComposeError::Podman(_)),
			"expected ComposeError::Podman, got: {err:?}"
		);
	}

	#[tokio::test]
	async fn a_lost_removal_response_succeeds_when_the_container_is_gone() {
		let fake = fake_dropping_the_op(None);
		let engine = engine_with(fake.client(), "proj");
		engine
			.teardown_one_container("proj-web-1", 10, &[], false)
			.await
			.expect("the container is absent, so the removal landed");
	}
}
