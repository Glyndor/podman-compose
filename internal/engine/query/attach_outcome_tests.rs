use super::AttachOutcome;

/// The two endings must stay distinguishable. They are the difference
/// between a CI job that ran to completion and one that was cancelled, and
/// before this existed both reported exit 0.
#[test]
fn the_two_endings_are_not_equal() {
	assert_ne!(AttachOutcome::StreamsEnded, AttachOutcome::Interrupted);
}

/// A truncated stream is its own ending, not either of the first two. Folding
/// it into `StreamsEnded` is what let an attached `up` lose its connection to
/// the engine and still exit 0.
#[test]
fn a_broken_stream_is_neither_of_the_other_two() {
	assert_ne!(AttachOutcome::StreamBroke, AttachOutcome::StreamsEnded);
	assert_ne!(AttachOutcome::StreamBroke, AttachOutcome::Interrupted);
}
