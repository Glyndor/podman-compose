//! What an attached `up` does when a log stream dies under it.
//!
//! The transport cannot say whether a body that stopped without its terminator
//! finished or broke (#1104, and `stream_end_tests` pins that both cuts arrive
//! the same way). The container's own state is the second, independent
//! observation that answers it, so these drive a real severed body against the
//! fake and vary only the container listing.
//!
//! `--abort-on-container-exit` and `--exit-code-from` (#1492) use the same
//! observation to fire on the first container to stop. Their tests live here
//! too, so the abort path is exercised against the same fake the truncation
//! paths use. The non-abort tests still call [`Engine::attach_logs_with_options`]
//! with the legacy two-parameter signature, so the delegation from that method
//! to [`Engine::attach_logs_with`] is covered on the path existing-attached-
//! `up` callers actually take.

#![cfg(unix)]

use super::inspect::{AttachOptions, AttachOutcome, AttachSummary};
use super::Engine;
use crate::compose::types::ComposeFile;
use crate::engine::fake_podman::{self, FakeReply};

/// One `tty: true` service, so attach reads the raw byte stream rather than the
/// multiplexed framing. The cut is what matters here, not the framing.
fn compose() -> crate::compose::types::ComposeFile {
	crate::parse_str("services:\n  app:\n    image: img\n    tty: true\n").unwrap()
}

/// Two services, both tty, so the abort path has a choice of which one exits
/// first and a name to match against `--exit-code-from`. Names matter: the test
/// fake keys the stream reply on the container name in the URL.
fn compose_two() -> ComposeFile {
	crate::parse_str(
		"services:\n  first:\n    image: img\n    tty: true\n  second:\n    \
		 image: img\n    tty: true\n",
	)
	.unwrap()
}

/// A fake whose log stream is cut with no terminating chunk, and whose container
/// listing reports `state`.
fn fake_with_state(state: &'static str) -> fake_podman::FakePodman {
	fake_podman::start_replying(move |_method, target| {
		if target.contains("/logs") {
			FakeReply::ChunkedTruncated(vec!["hello from the container\n".to_string()])
		} else if target.contains("/containers/json") {
			FakeReply::Body(
				200,
				format!(r#"[{{"Names":["/proj-app-1"],"State":"{state}"}}]"#),
			)
		} else {
			FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
		}
	})
}

fn engine(fake: &fake_podman::FakePodman) -> Engine {
	Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir())
}

/// Fake that ends both services' log streams cleanly, lists both as exited, and
/// answers `/wait` with the per-container exit code passed in. Stop calls
/// return 204 (idempotent on already-stopped containers is fine — the abort
/// path's `engine.stop` runs against an already-exited project).
fn fake_two_services_exited(first_code: i64, second_code: i64) -> fake_podman::FakePodman {
	fake_podman::start_replying(move |method, target| {
		// Match the full project-prefixed container name; `target.contains`
		// against `first-1` would also match `proj-first-1`, so the prefix
		// keeps the two services' routes unambiguous.
		if method == "GET" && target.contains("/containers/proj-first-1/logs") {
			FakeReply::ChunkedTruncated(vec!["first ran\n".to_string()])
		} else if method == "GET" && target.contains("/containers/proj-second-1/logs") {
			FakeReply::ChunkedTruncated(vec!["second ran\n".to_string()])
		} else if method == "POST" && target.contains("proj-first-1/wait") {
			FakeReply::Body(200, first_code.to_string())
		} else if method == "POST" && target.contains("proj-second-1/wait") {
			FakeReply::Body(200, second_code.to_string())
		} else if target.contains("/containers/json") {
			// Both containers are listed as exited — the abort path's
			// `container_still_running` will answer "stopped" for either.
			FakeReply::Body(
				200,
				r#"[{"Names":["/proj-first-1"],"State":"exited"},
				     {"Names":["/proj-second-1"],"State":"exited"}]"#
					.to_string(),
			)
		} else if method == "POST" {
			// Stop calls (POST /containers/X/stop?t=...) — 204 makes them
			// idempotent no-ops against the already-stopped containers, which
			// is what `engine.stop` does when the project is already down.
			FakeReply::Body(204, String::new())
		} else {
			FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
		}
	})
}

/// Non-abort path: kept on the 4.0.0 two-parameter form so the legacy
/// delegation through [`Engine::attach_logs_with_options`] is exercised here,
/// not just inside the wrapper itself.
#[tokio::test]
async fn a_stream_cut_while_the_container_runs_is_a_broken_stream() {
	let fake = fake_with_state("running");
	let outcome = engine(&fake)
		.attach_logs_with_options(&compose(), false)
		.await
		.expect("attach itself must not error; the outcome carries the verdict");

	assert_eq!(
		outcome,
		AttachOutcome::StreamBroke,
		"a cut body with the container still running truncated live output"
	);
}

/// Non-abort path: kept on the 4.0.0 two-parameter form for the same reason as
/// `a_stream_cut_while_the_container_runs_is_a_broken_stream`.
#[tokio::test]
async fn a_stream_cut_as_the_container_stopped_is_a_clean_end() {
	let fake = fake_with_state("exited");
	let outcome = engine(&fake)
		.attach_logs_with_options(&compose(), false)
		.await
		.expect("attach itself must not error");

	assert_eq!(
		outcome,
		AttachOutcome::StreamsEnded,
		"the container stopped, so the stream had every reason to end: a missing \
		 terminator must not fail an `up` that finished"
	);
}

/// Abort path: uses the 4.1.0 [`Engine::attach_logs_with`] entry point and
/// reads the exit-code metadata off [`AttachSummary`], the new home for the
/// fields the 4.0.0 `Aborted` struct variant carried.
#[tokio::test]
async fn abort_on_container_exit_fires_on_first_exited_container() {
	let fake = fake_two_services_exited(7, 9);
	let summary = engine(&fake)
		.attach_logs_with(
			&compose_two(),
			&AttachOptions::new().with_abort_on_container_exit(true),
		)
		.await
		.expect("abort path must not error; the summary carries the verdict");

	// The first container's stream to end wins. The stream from
	// `proj-first-1` was opened first by the engine, so it is the one whose
	// chunked body finishes first in the `FuturesUnordered` loop. Whichever
	// service wins, the exit code must match that container's `/wait`.
	assert_eq!(summary.outcome, AttachOutcome::Aborted);
	let service = summary
		.service
		.as_deref()
		.expect("abort summary carries the trigger service");
	let exit_code = summary
		.exit_code
		.expect("abort summary carries the propagated exit code");
	assert!(
		(service == "first" && exit_code == 7) || (service == "second" && exit_code == 9),
		"trigger service and exit code must agree (got {service} / {exit_code})"
	);
}

/// Pair to the rejection test below. `--exit-code-from app` is accepted because
/// `app` is defined in the compose file; the abort path is what the variant
/// classification is being asserted against, not the validation.
#[tokio::test]
async fn abort_with_exit_code_from_returns_named_service_exit_code() {
	let fake = fake_two_services_exited(7, 9);
	let summary = engine(&fake)
		.attach_logs_with(
			&compose_two(),
			&AttachOptions::new()
				.with_abort_on_container_exit(true)
				.with_exit_code_from(Some("second".to_string())),
		)
		.await
		.expect("exit-code-from against a known service must be accepted");

	// `--exit-code-from second` always returns second's exit code, regardless
	// of which container's stream finished first. The 9 here is what docker
	// compose v5.1.3 returns for the same scenario (measured against the same
	// Podman socket).
	assert_eq!(
		summary,
		AttachSummary {
			outcome: AttachOutcome::Aborted,
			service: Some("second".to_string()),
			exit_code: Some(9),
		}
	);
}

/// `--exit-code-from ghost` names a service that does not exist in the compose
/// file. The check runs before any container is created so the error surfaces
/// as a clear `ServiceNotFound`, not a generic `is_err()` that could mean
/// anything. Asserts the variant — a bare `is_err()` would be satisfied by
/// any failure.
#[tokio::test]
async fn exit_code_from_with_unknown_service_is_rejected() {
	let fake = fake_two_services_exited(0, 0);
	let err = engine(&fake)
		.attach_logs_with(
			&compose_two(),
			&AttachOptions::new()
				.with_abort_on_container_exit(true)
				.with_exit_code_from(Some("ghost".to_string())),
		)
		.await
		.expect_err("--exit-code-from ghost must be rejected up front");
	assert!(
		matches!(err, crate::ComposeError::ServiceNotFound(ref s) if s == "ghost"),
		"got: {err:?}"
	);
}

/// Pair to the rejection test above. `--exit-code-from first` is just inside
/// the limit (the named service exists) and must not be rejected at the
/// validation gate; what the function then returns is the abort's exit code.
#[tokio::test]
async fn exit_code_from_with_known_service_is_accepted() {
	let fake = fake_two_services_exited(7, 9);
	let summary = engine(&fake)
		.attach_logs_with(
			&compose_two(),
			&AttachOptions::new()
				.with_abort_on_container_exit(true)
				.with_exit_code_from(Some("first".to_string())),
		)
		.await
		.expect("--exit-code-from first (a defined service) must not be rejected");
	assert_eq!(
		summary,
		AttachSummary {
			outcome: AttachOutcome::Aborted,
			service: Some("first".to_string()),
			exit_code: Some(7),
		}
	);
}

/// Without `--abort-on-container-exit` (and without `--exit-code-from`), a
/// container that exits mid-stream is **not** an event — we keep waiting for
/// the others, and the function returns `StreamsEnded` when they finish. This
/// is the existing-attached-`up` behavior, kept intact.
///
/// Stays on the 4.0.0 two-parameter form so the delegation through
/// [`Engine::attach_logs_with_options`] is exercised here as well.
#[tokio::test]
async fn container_exit_without_abort_flag_does_not_trigger_abort() {
	let fake = fake_two_services_exited(7, 9);
	let outcome = engine(&fake)
		.attach_logs_with_options(&compose_two(), false)
		.await
		.expect("attach must not error");

	assert_eq!(
		outcome,
		AttachOutcome::StreamsEnded,
		"without --abort-on-container-exit, a container exiting mid-stream is just \
		 one more stream that finished"
	);
}
