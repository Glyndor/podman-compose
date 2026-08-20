//! Unit and engine-level tests for the `pull` path: dependency-closure
//! expansion, `pull_policy` resolution, and the de-duplication that collapses
//! a shared image into a single request.
//!
//! Split out of `build/pull.rs` so the production code stays under the
//! source-line limit, the same way `pull_typo_tests.rs` already is.

use super::{libpod_pull_policy, pull_dep_closure};

#[test]
fn dep_closure_includes_transitive_dependencies() {
	let file = crate::parse_str(
		"services:\n  web:\n    image: a\n    depends_on:\n      - api\n  api:\n    image: b\n    depends_on:\n      - db\n  db:\n    image: c\n  lone:\n    image: d\n",
	)
	.unwrap();
	let mut got: Vec<String> = pull_dep_closure(&file, &["web".to_string()])
		.into_iter()
		.collect();
	got.sort();
	assert_eq!(got, vec!["api", "db", "web"]);
}

#[test]
fn dep_closure_of_leaf_is_just_itself() {
	let file = crate::parse_str("services:\n  db:\n    image: c\n").unwrap();
	let got: Vec<String> = pull_dep_closure(&file, &["db".to_string()])
		.into_iter()
		.collect();
	assert_eq!(got, vec!["db"]);
}

#[tokio::test]
async fn pull_unknown_service_is_rejected() {
	// `pull bogus` must error on the unknown name instead of silently exiting 0.
	let file = crate::parse_str("services:\n  web:\n    image: a\n").unwrap();
	let e = crate::engine::Engine::new(
		crate::libpod::Client::new("/nonexistent.sock"),
		"proj".into(),
	);
	let err = e
		.pull_services(&file, &["nope".to_string()])
		.await
		.expect_err("unknown service must be rejected");
	assert!(
		matches!(err, crate::error::ComposeError::ServiceNotFound(_)),
		"unexpected error: {err:?}"
	);
}

// `pull_rejects_an_unknown_pull_policy_without_panicking` lives in the
// sibling `pull_typo_tests.rs` to keep this file below the source-line
// limit (#1450).

#[test]
fn pull_policy_maps_every_spec_value() {
	assert_eq!(libpod_pull_policy(Some("always")), Some("always"));
	assert_eq!(libpod_pull_policy(Some("newer")), Some("newer"));
	assert_eq!(libpod_pull_policy(Some("never")), Some("never"));
	assert_eq!(libpod_pull_policy(Some("missing")), Some("missing"));
	// `if_not_present` is the spec alias for `missing`.
	assert_eq!(libpod_pull_policy(Some("if_not_present")), Some("missing"));
	assert_eq!(libpod_pull_policy(Some("build")), Some("missing"));
	assert_eq!(libpod_pull_policy(None), Some("missing"));
	// Unknown values are reported (None) so the caller fails loud (#1369).
	assert_eq!(libpod_pull_policy(Some("bogus")), None);
}

/// A typo'd `pull_policy:` must surface as a hard error (#1369): the
/// previous warn-and-default-to-missing path silently turned `alaways`
/// into the opposite of what the user wrote, and a `never` typo pulled
/// fresh images on every `up` without telling the operator.
#[test]
fn resolved_pull_policy_rejects_an_unknown_value() {
	let e = crate::engine::Engine::new(
		crate::libpod::Client::new("/nonexistent.sock"),
		"proj".into(),
	);
	let svc = crate::compose::types::Service {
		image: Some("nginx:1.27".to_string()),
		pull_policy: Some("alaways".to_string()),
		..crate::compose::types::Service::default()
	};
	let err = e.resolved_pull_policy("web", &svc).unwrap_err();
	let msg = err.to_string();
	assert!(msg.contains("alaways"), "got {msg}");
	assert!(msg.contains("always"), "got {msg}");
	assert!(
		matches!(
			err,
			crate::error::ComposeError::Podman(crate::libpod::PodmanError::Field {
				ref service,
				ref field,
				..
			}) if service == "web" && field == "pull_policy"
		),
		"unknown pull policy must surface as a Field error carrying the service and field name, got {err:?}"
	);
}

#[cfg(unix)]
use crate::engine::fake_podman;

/// #8: `pull_services_with_options` used to build one future per service,
/// so two services sharing an image pulled it twice. They must now
/// dedupe down to a single pull request, with both services still
/// reported as successful.
#[tokio::test]
#[cfg(unix)]
async fn pull_dedupes_a_shared_image_into_a_single_pull() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
			(200, "{}".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file =
		crate::parse_str("services:\n  a:\n    image: shared\n  b:\n    image: shared\n").unwrap();

	e.pull_services(&file, &[])
		.await
		.expect("pulling two services that share an image must succeed");

	let seen = fake.requests.lock().unwrap();
	let pulls = seen
		.iter()
		.filter(|r| r.contains("/images/pull") && r.contains("reference=shared"))
		.count();
	assert_eq!(
		pulls, 1,
		"two services sharing one image must issue a single pull: {seen:?}"
	);
}

/// Two services sharing an image but with *different* resolved pull
/// policies (no `--pull` override) must each get their own pull request,
/// not collapse into one — the dedup key must include the resolved
/// policy, not just the image reference.
#[tokio::test]
#[cfg(unix)]
async fn pull_issues_separate_requests_for_same_image_different_policy() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
			(200, "{}".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file = crate::parse_str(
		"services:\n  a:\n    image: shared\n    pull_policy: never\n  b:\n    image: shared\n    pull_policy: always\n",
	)
	.unwrap();

	e.pull_services(&file, &[])
		.await
		.expect("differing per-service pull_policy must not fail the pull");

	let seen = fake.requests.lock().unwrap();
	let pulls: Vec<&String> = seen
		.iter()
		.filter(|r| r.contains("/images/pull") && r.contains("reference=shared"))
		.collect();
	assert_eq!(
		pulls.len(),
		2,
		"same image with different resolved policies must issue two pulls, not one: {seen:?}"
	);
	assert!(
		pulls.iter().any(|r| r.contains("policy=never")),
		"missing the never-policy pull: {seen:?}"
	);
	assert!(
		pulls.iter().any(|r| r.contains("policy=always")),
		"missing the always-policy pull: {seen:?}"
	);
}

/// A shared image that fails to pull must still be reported for *every*
/// service that names it — derived from the one shared outcome, not from
/// a redundant pull per service. `ignore_failures` lets both warnings
/// through instead of aborting on the first.
#[tokio::test]
#[cfg(unix)]
async fn pull_failure_on_a_shared_image_is_still_only_pulled_once() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(500, r#"{"message":"registry unreachable"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file =
		crate::parse_str("services:\n  a:\n    image: shared\n  b:\n    image: shared\n").unwrap();

	let opts = super::PullOptions {
		ignore_failures: true,
		include_deps: false,
	};
	e.pull_services_with_options(&file, &[], opts)
		.await
		.expect("ignore_failures must not error even though the shared pull failed");

	let seen = fake.requests.lock().unwrap();
	let pulls = seen
		.iter()
		.filter(|r| r.contains("/images/pull") && r.contains("reference=shared"))
		.count();
	assert_eq!(
		pulls, 1,
		"a failing shared image must still be pulled once, not once per service: {seen:?}"
	);
}

/// Without `ignore_failures`, a shared image that never lands must still
/// abort the whole pull — the per-service error report is derived from
/// the image's single shared outcome, so the failure is not silently
/// dropped for services 2..N once service 1 already reported it.
#[tokio::test]
#[cfg(unix)]
async fn pull_failure_on_a_shared_image_aborts_without_ignore_failures() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(500, r#"{"message":"registry unreachable"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file =
		crate::parse_str("services:\n  a:\n    image: shared\n  b:\n    image: shared\n").unwrap();

	let err = e
		.pull_services(&file, &[])
		.await
		.expect_err("a shared image that fails to pull must abort the pull");
	assert!(
		matches!(err, crate::error::ComposeError::Build(ref msg) if msg.contains("shared")),
		"unexpected error: {err:?}"
	);
}

// Bounding the standalone pull's concurrency (`MAX_PULL_CONCURRENCY`) is
// exercised structurally rather than by asserting a live in-flight count:
// `bounded_join_all` runs every future through the same
// `buffer_unordered(MAX_PULL_CONCURRENCY)` dispatcher the lifecycle
// fan-out's `join_bounded` uses (see `parallel::tests::
// join_bounded_preserves_input_order`), and a synchronous fake responder
// cannot observe real concurrency without a multi-thread runtime and a
// blocking rendezvous — exactly the flakiness the testing standard rules
// out. The dedup tests above already pin the dispatch contract (every
// unique image is attempted, exactly once).

/// #1076: libpod reports a failed pull as an in-band `error` line on a 200
/// response. That line used to be warned about and dropped, so every caller
/// believed the pull had succeeded.
///
/// The image is present here — a stale copy from an earlier pull — which is
/// exactly the case a presence probe cannot catch: it passes while the pull
/// failed. `up --pull always` against an unreachable registry therefore
/// started yesterday's image and exited 0.
#[tokio::test]
#[cfg(unix)]
async fn a_failed_pull_is_reported_even_when_a_stale_image_is_present() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			// 200 with an in-band error, the way libpod reports it.
			(
				200,
				r#"{"error":"initializing source: pinging container registry: connection refused"}"#
					.to_string(),
			)
		} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
			// The stale image is in local storage.
			(200, "{}".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file = crate::parse_str("services:\n  a:\n    image: stale:v1\n").unwrap();

	let err = e
		.pull_services(&file, &[])
		.await
		.expect_err("a pull that libpod reported as failed must not be reported as success");
	assert!(
		format!("{err}").contains("connection refused"),
		"the underlying cause must survive: {err}"
	);
}

/// The escape hatch still works: `--ignore-pull-failures` is deliberately
/// exit-0, and that must not change just because the failure is now visible.
#[tokio::test]
#[cfg(unix)]
async fn ignore_pull_failures_still_exits_zero_on_an_in_band_error() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(200, r#"{"error":"connection refused"}"#.to_string())
		} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
			(200, "{}".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file = crate::parse_str("services:\n  a:\n    image: stale:v1\n").unwrap();

	e.pull_services_with_options(
		&file,
		&[],
		crate::engine::PullOptions {
			ignore_failures: true,
			..Default::default()
		},
	)
	.await
	.expect("--ignore-pull-failures must stay exit 0");
}
