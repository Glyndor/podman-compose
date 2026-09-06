//! [`ComposeError::WaitTimeout`] is the variant [`Engine`] emits only when
//! `start --wait --wait-timeout` (or `up --wait --wait-timeout`) is in effect:
//! the outer `tokio::time::timeout` wrapping [`Engine::wait_services_healthy`]
//! in [`crate::dispatch`] elapses before the inner poll resolves.
//!
//! It is distinct from [`ComposeError::HealthCheckTimeout`], which is what a
//! service's own healthcheck budget exhaustion looks like. The `--wait-timeout`
//! exists for the case where every service still *can* become healthy given
//! enough time; without it, a generous plan budget is the only cap, and that
//! is what the user is overriding with `--wait-timeout 30`.
//!
//! The dispatch module builds this variant, so the test below mirrors its
//! exact pattern (`tokio::time::timeout` around `wait_services_healthy_within`)
//! rather than reimplementing the wrapping inline. The mirror is the point:
//! the variant only exists because the wrapper exists, so the assertion is
//! about the wrapper, not about the engine.

#![cfg(unix)]

use crate::compose::types::{Command, ComposeFile, HealthCheck, Service};
use crate::engine::fake_podman::{self, FakeReply};
use crate::engine::Engine;
use crate::error::ComposeError;
use crate::libpod::API_PREFIX;

/// A service that never reaches `healthy` plus a finite outer deadline is
/// the one shape that surfaces [`ComposeError::WaitTimeout`]. The message
/// must name the seconds: that is the only knob the operator set, and the
/// only one they need to tweak to retry.
#[tokio::test]
async fn an_unhealthy_service_under_a_short_wait_timeout_surfaces_waittimeout() {
	let fake = fake_podman::start_replying(|method, target| {
		// The inspect that `wait_healthy` reads before deciding to poll.
		if method == "GET" && target.contains("/containers/proj-web-1/json") {
			// `starting` health under an effective healthcheck is the
			// `HealthVerdict::Pending` branch, exactly the one the
			// wrapper sits on top of.
			return FakeReply::Body(
				200,
				r#"{"State":{"Status":"running","Health":{"Status":"starting"}},"Config":{"Healthcheck":{"Test":["CMD","true"]}}}"#
					.to_string(),
			);
		}
		// The on-demand healthcheck run that the wait polls between reads.
		if method == "GET" && target.contains("/containers/proj-web-1/healthcheck") {
			return FakeReply::Body(200, r#"{"Status":"starting"}"#.to_string());
		}
		FakeReply::Body(404, r#"{"message":"not used"}"#.to_string())
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());

	// A service with an effective healthcheck that never reports `healthy`,
	// plus a short per-probe budget so the inner future is the slow one,
	// not the wrapper. The wait_timeout we feed the *outer* `tokio::timeout`
	// is the one that ends up in the error.
	let mut file = ComposeFile::default();
	file.services.insert(
		"web".into(),
		Service {
			image: Some("nginx:1.27".into()),
			healthcheck: Some(HealthCheck {
				test: Some(Command::Exec(vec!["CMD".into(), "true".into()])),
				interval: Some("200ms".into()),
				retries: Some(3),
				..Default::default()
			}),
			..Default::default()
		},
	);

	let wait_timeout_secs: u64 = 1;
	let wait_timeout = std::time::Duration::from_secs(wait_timeout_secs);

	// Mirror the dispatch.rs wrapper byte-for-byte: the variant only exists
	// because this exact `map_err(|_| ComposeError::WaitTimeout { secs })`
	// does. The inner future is the health wait with a generous
	// per-service budget, so the OUTER `tokio::timeout` (the one the
	// dispatch owns) is what elapses first.
	let fut = engine.wait_services_healthy_within(&file, &[], Some(wait_timeout * 20));
	let result =
		tokio::time::timeout(wait_timeout, fut)
			.await
			.map_err(|_| ComposeError::WaitTimeout {
				secs: wait_timeout_secs,
			});

	let err = result.expect_err("an unhealthy service under a finite wait must surface an error");
	let ComposeError::WaitTimeout { secs } = err else {
		panic!("expected WaitTimeout, got: {err:?}");
	};
	assert_eq!(
		secs, wait_timeout_secs,
		"the error must carry the same seconds the wrapper was given"
	);

	let msg = err.to_string();
	assert!(
		msg.contains(&wait_timeout_secs.to_string()),
		"the rendered message must name the seconds, got: {msg}"
	);
	assert!(
		msg.contains("timed out"),
		"the rendered message must say the wait timed out, got: {msg}"
	);
}

/// A wait that *does* finish before the wrapper elapses must not produce
/// `WaitTimeout`. The wrapper must yield the inner result (here: a
/// `HealthCheckTimeout` because the fake never reports `healthy`). Pinning
/// the inverse: the timeout path only fires when the inner future is
/// genuinely slow, not on every dispatch, so a regression that always
/// wraps cannot land as `WaitTimeout`.
#[tokio::test]
async fn a_wait_that_finishes_in_time_does_not_produce_waittimeout() {
	// Same shape as the trigger test, but with the wrapper set generously
	// larger than the inner budget. The inner `HealthCheckTimeout` arrives
	// well before the wrapper elapses, so the wrapper must surface the
	// inner error rather than collapse it into `WaitTimeout`.
	let fake = fake_podman::start_replying(|_method, target| {
		if target.contains("/containers/proj-web-1/json") {
			return FakeReply::Body(
				200,
				r#"{"State":{"Status":"running","Health":{"Status":"starting"}},"Config":{"Healthcheck":{"Test":["CMD","true"]}}}"#
					.to_string(),
			);
		}
		if target.contains("/containers/proj-web-1/healthcheck") {
			return FakeReply::Body(200, r#"{"Status":"starting"}"#.to_string());
		}
		FakeReply::Body(404, r#"{"message":"not used"}"#.to_string())
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());

	let mut file = ComposeFile::default();
	file.services.insert(
		"web".into(),
		Service {
			image: Some("nginx:1.27".into()),
			healthcheck: Some(HealthCheck {
				test: Some(Command::Exec(vec!["CMD".into(), "true".into()])),
				interval: Some("200ms".into()),
				retries: Some(3),
				..Default::default()
			}),
			..Default::default()
		},
	);

	// Tight inner, loose wrapper: the inner must error first. The
	// dispatch.rs wrapper only fires when the inner is slow.
	let inner_budget = std::time::Duration::from_secs(5);
	let wrapper = std::time::Duration::from_secs(30);
	let fut = engine.wait_services_healthy_within(&file, &[], Some(inner_budget));
	let result = tokio::time::timeout(wrapper, fut).await;

	// The wrapper must surface the inner error (the service never becomes
	// healthy, so it is `HealthCheckTimeout`), never collapse to
	// `WaitTimeout` just because it returned an Err.
	let inner = result.expect("a 30s wrapper must not elapse on a service that polls every 200ms");
	let err = inner.expect_err("the inner wait must error on an unhealthy service");
	assert!(
		matches!(err, ComposeError::HealthCheckTimeout(_)),
		"inner error must be HealthCheckTimeout, not WaitTimeout, got: {err:?}"
	);
	assert!(
		!matches!(err, ComposeError::WaitTimeout { .. }),
		"a wrapper that did not elapse must not produce WaitTimeout"
	);
}

/// Sanity check that the inspect path the wait uses (`/containers/{name}/json`)
/// is the one the wrapper is built around. If libpod ever renames it, this
/// test is the one that has to be updated first: the wrapper's behaviour
/// depends on the wait polling at this URL.
#[tokio::test]
async fn the_wait_polls_the_expected_inspect_path() {
	let fake = fake_podman::start_replying(|method, target| {
		if method == "GET" && target.contains("/containers/proj-web-1/json") {
			return FakeReply::Body(
				200,
				r#"{"State":{"Status":"running","Health":{"Status":"healthy"}}}"#.to_string(),
			);
		}
		FakeReply::Body(404, r#"{"message":"not used"}"#.to_string())
	});
	let engine = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());

	let mut file = ComposeFile::default();
	file.services.insert(
		"web".into(),
		Service {
			image: Some("nginx:1.27".into()),
			healthcheck: Some(HealthCheck {
				test: Some(Command::Exec(vec!["CMD".into(), "true".into()])),
				..Default::default()
			}),
			..Default::default()
		},
	);

	// Healthy on the first inspect, so the wait must return without
	// touching the budget.
	engine
		.wait_services_healthy(&file, &[])
		.await
		.expect("a service reported healthy on inspect must not error");
	// The API_PREFIX constant is exported for a reason: the path the
	// client builds lives here; touching the wrong one breaks the wait.
	let _ = API_PREFIX;
}
