//! What a streaming read looks like when it ends well, and when it does not.
//!
//! #1104 is the question of whether podup can tell those apart. The argument has
//! been circular so far because the only evidence came from a live daemon, where
//! *both* the version and the wire shape move at once: making a mid-stream error
//! fatal reddened fifteen tests on the lane's Podman 5.8.1 while the identical
//! commit stayed green on 5.4.2.
//!
//! These tests take the daemon out of it. The fake writes the wire shapes
//! deliberately, so what podup does with each is pinned here rather than inferred
//! from a lane run.
//!
//! Two things they settle:
//!
//! 1. **podup's parsing is not the ambiguity.** A properly terminated chunked body
//!    reaches the caller as a clean end, every time. So when a *finished* stream
//!    arrives as an error on some Podman version, the framing came that way: no
//!    predicate on `hyper::Error` can invent a difference the server did not send.
//!    That is the case for the out-of-band re-check `logs` and `stats` already use,
//!    and for `events` needing one of its own.
//! 2. **A severed stream is not `IncompleteMessage`.** That predicate is about the
//!    message head. Both places a cut can land in a body arrive as a hyper Body
//!    error wrapping `io::ErrorKind::UnexpectedEof`, which used to fall through to
//!    `hyper-other`. Anything keyed on the `IncompleteMessage` text, including the
//!    lane's own flake counter, cannot see them.

#![cfg(unix)]

use futures_util::StreamExt;

use super::fake_podman::{self, FakeReply};

/// Drive one streaming GET against the fake and collect what the parser yields:
/// the frames that arrived, and how the stream ended (`None` for a clean end).
async fn read_stream(reply: FakeReply) -> (Vec<serde_json::Value>, Option<String>) {
	let fake = fake_podman::start_replying(move |_method, _target| match &reply {
		FakeReply::Body(s, b) => FakeReply::Body(*s, b.clone()),
		FakeReply::ChunkedEnd(c) => FakeReply::ChunkedEnd(c.clone()),
		FakeReply::ChunkedTruncated(c) => FakeReply::ChunkedTruncated(c.clone()),
		FakeReply::ChunkedCutMidPayload(c) => FakeReply::ChunkedCutMidPayload(c.clone()),
		// Not a stream *ending*; it never becomes a stream. `get_stream` fails
		// at the response head rather than reaching the parser this measures, so
		// this shape belongs to the lifecycle re-check tests instead.
		FakeReply::ClosedWithoutResponse => FakeReply::ClosedWithoutResponse,
	});
	let client = fake.client();
	let resp = client
		.get_stream(&format!("{}/events", crate::libpod::API_PREFIX))
		.await
		.expect("the fake answers 200");
	let mut stream = crate::libpod::parse_json_lines::<serde_json::Value>(resp.into_body());

	let mut frames = Vec::new();
	let mut ended_as = None;
	while let Some(item) = stream.next().await {
		match item {
			Ok(value) => frames.push(value),
			Err(e) => {
				// The classification podup would report, named rather than argued.
				ended_as = Some(e.stream_end_kind().to_string());
				break;
			}
		}
	}
	(frames, ended_as)
}

fn frame(action: &str) -> String {
	format!("{{\"Type\":\"container\",\"Action\":\"{action}\"}}\n")
}

/// A stream that ends the way HTTP says it should reaches the caller as a clean
/// end, with no error at all. Every frame written arrives first.
#[tokio::test]
async fn a_properly_terminated_stream_ends_clean() {
	let (frames, ended_as) =
		read_stream(FakeReply::ChunkedEnd(vec![frame("start"), frame("die")])).await;

	assert_eq!(frames.len(), 2, "both frames arrive: {frames:?}");
	assert_eq!(
		ended_as, None,
		"a terminated chunked body must not surface as an error, it surfaced as {ended_as:?}"
	);
}

/// A body cut off between chunks: the connection closes where the next chunk
/// header should begin.
#[tokio::test]
async fn a_cut_between_chunks_is_a_body_eof() {
	let (frames, ended_as) = read_stream(FakeReply::ChunkedTruncated(vec![frame("start")])).await;

	assert_eq!(
		frames.len(),
		1,
		"the frame written before the cut still arrives: {frames:?}"
	);
	assert_eq!(
		ended_as.as_deref(),
		Some("body-unexpected-eof"),
		"hyper reports this as a Body error wrapping UnexpectedEof \
		 (\"unexpected EOF during chunk size line\"), not IncompleteMessage"
	);
}

/// And a body cut mid-payload: the chunk header promises bytes that never come.
/// hyper words it differently (`IncompleteBody`) but the io kind is the same, and
/// the kind is what podup keys on.
#[tokio::test]
async fn a_cut_mid_payload_is_also_a_body_eof() {
	let (_frames, ended_as) = read_stream(FakeReply::ChunkedCutMidPayload(format!(
		"{{\"Type\":\"container\",\"Action\":\"start\",\"padding\":\"{}\"}}\n",
		"a".repeat(64)
	)))
	.await;

	assert_eq!(ended_as.as_deref(), Some("body-unexpected-eof"));
}

/// Neither cut is `IncompleteMessage`. Its own test because the lane's flake
/// counter greps for that word (plus "connection closed before message
/// completed", "channel closed" and "Connection reset"), so a real mid-body drop
/// is currently counted as a genuine failure rather than a transport flake.
#[tokio::test]
async fn neither_cut_is_an_incomplete_message() {
	for reply in [
		FakeReply::ChunkedTruncated(vec![frame("start")]),
		FakeReply::ChunkedCutMidPayload(format!("{{\"padding\":\"{}\"}}\n", "a".repeat(64))),
	] {
		let (_frames, ended_as) = read_stream(reply).await;
		assert_ne!(
			ended_as.as_deref(),
			Some("incomplete-message"),
			"a severed body is not the head-level IncompleteMessage"
		);
	}
}

/// The crux of #1104, as a test rather than a comment: the payload delivered is
/// identical either way, so a caller that only looks at whether an error arrived
/// cannot tell a finished stream from a severed one. Telling them apart needs a
/// second, out-of-band observation: is the thing this stream was following still
/// alive?
#[tokio::test]
async fn the_two_shapes_carry_the_same_payload() {
	let (clean_frames, clean_end) = read_stream(FakeReply::ChunkedEnd(vec![frame("start")])).await;
	let (severed_frames, severed_end) =
		read_stream(FakeReply::ChunkedTruncated(vec![frame("start")])).await;

	assert_eq!(clean_frames, severed_frames, "same frames delivered");
	assert!(clean_end.is_none(), "clean end: {clean_end:?}");
	assert!(severed_end.is_some(), "severed end must be an error");
}
