//! What an attached `up` does when a log stream dies under it.
//!
//! The transport cannot say whether a body that stopped without its terminator
//! finished or broke (#1104, and `stream_end_tests` pins that both cuts arrive
//! the same way). The container's own state is the second, independent
//! observation that answers it, so these drive a real severed body against the
//! fake and vary only the container listing.

#![cfg(unix)]

use super::inspect::AttachOutcome;
use super::Engine;
use crate::engine::fake_podman::{self, FakeReply};

/// One `tty: true` service, so attach reads the raw byte stream rather than the
/// multiplexed framing. The cut is what matters here, not the framing.
fn compose() -> crate::compose::types::ComposeFile {
	crate::parse_str("services:\n  app:\n    image: img\n    tty: true\n").unwrap()
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
