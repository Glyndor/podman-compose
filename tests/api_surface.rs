//! The public surface an embedding daemon needs, exercised by naming it.
//!
//! helmly-agent and epistle consume podup as a library. When a call fails they
//! receive a `ComposeError::Podman`, and deciding whether to retry means asking
//! the error what kind of failure it was. Before #1474 the type inside was not
//! nameable from outside the crate and every predicate was `pub(crate)`, so the
//! only way to ask was matching on the message text.
//!
//! This file is a compile-time contract, not a behavioural test: it fails by
//! not building. Nothing here asserts what the predicates return — only that a
//! caller outside the crate can name the type and call them.

use podup::{ComposeError, LogOutput, PodmanError};

/// Name every type the retry path of an embedding daemon has to write down.
#[test]
fn public_types_are_nameable() {
	fn _takes_error(_: &PodmanError) {}
	fn _takes_frame(_: &LogOutput) {}
	fn _takes_compose_error(_: &ComposeError) {}
}

/// Call each predicate the engine's own retry logic uses. A consumer that
/// cannot reach these has to fall back to `err.to_string().contains(..)`,
/// which breaks the day libpod rewords a message — and breaks silently,
/// because the retry simply stops happening.
#[test]
fn retry_predicates_are_callable_from_outside() {
	fn _probe(e: &PodmanError) -> (bool, bool, bool, bool, bool, bool, bool, &'static str) {
		(
			e.is_timeout(),
			e.is_incomplete_message(),
			e.is_kill_of_stopped(),
			e.is_already_exists(),
			e.is_image_in_use(),
			e.is_state_conflict(),
			// The only predicate that takes an argument, and the only one this
			// file did not name until #1502. A daemon deciding whether to retry
			// asks about a specific status - 409 is worth another try after a
			// backoff, 404 never is - so it belongs in the contract as much as
			// the nullary ones.
			e.is_status(409),
			e.stream_end_kind(),
		)
	}
}

/// The stream parsers, for a consumer routing container output somewhere other
/// than this process's stdout — a TUI, a WebSocket, a log shipper.
#[test]
fn stream_parsers_are_reachable() {
	let _ = podup::parse_json_lines::<serde_json::Value>;
	let _ = podup::parse_multiplexed;
	let _ = podup::parse_raw;
}
