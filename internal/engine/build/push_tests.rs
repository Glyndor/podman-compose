use super::{drain_push_stream, ImagePullProgress};
use crate::error::ComposeError;
use crate::libpod::PodmanError;
use futures_util::StreamExt;
use std::time::Duration;

fn progress(stream: &str, error: &str) -> ImagePullProgress {
	ImagePullProgress {
		stream: stream.to_string(),
		error: error.to_string(),
	}
}

#[tokio::test]
async fn drain_ok_when_stream_completes_cleanly() {
	let items = vec![Ok(progress("pushing", "")), Ok(progress("done", ""))];
	let stream = futures_util::stream::iter(items);
	drain_push_stream(stream, "img", false, Duration::from_secs(5))
		.await
		.unwrap();
}

#[tokio::test]
async fn drain_surfaces_mid_stream_error_line() {
	let items = vec![Ok(progress("", "denied: unauthorized"))];
	let stream = futures_util::stream::iter(items);
	let err = drain_push_stream(stream, "img", false, Duration::from_secs(5))
		.await
		.unwrap_err();
	assert!(matches!(err, ComposeError::Build(m) if m.contains("denied: unauthorized")));
}

#[tokio::test]
async fn drain_times_out_on_an_unresponsive_stream() {
	// A stream that yields one line then never another stands in for a registry
	// that accepts the request then stalls; the per-line deadline must fire.
	let first = futures_util::stream::iter(vec![Ok(progress("pushing", ""))]);
	let stream = first.chain(futures_util::stream::pending::<
		std::result::Result<ImagePullProgress, PodmanError>,
	>());
	let err = drain_push_stream(stream, "img", false, Duration::from_millis(20))
		.await
		.unwrap_err();
	assert!(matches!(err, ComposeError::Build(m) if m.contains("no progress")));
}
