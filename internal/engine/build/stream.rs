//! Stream a build-context tar to the libpod build endpoint without buffering
//! the whole context in memory.
//!
//! [`context_body`] runs the (blocking) tar+gzip writer on a `spawn_blocking`
//! thread that feeds a bounded channel, and returns an async body stream that
//! drains it. Peak memory is roughly `CHANNEL_CAP * CHUNK_BYTES` regardless of
//! context size: a multi-gigabyte context no longer drives the process's RSS.

use std::io::{self, Write};
use std::path::PathBuf;

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use hyper::body::Frame;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::context::{stream_build_context, stream_build_context_with_inline};
use crate::error::Result;

/// Items handed to the request body: a data frame, or a terminal error that
/// aborts the upload.
type BodyItem = io::Result<Frame<Bytes>>;

/// How many pending chunks the channel buffers. Bounds peak memory to about
/// `CHANNEL_CAP * CHUNK_BYTES` while still decoupling the blocking tar writer
/// from the async socket writer so both run concurrently.
const CHANNEL_CAP: usize = 8;

/// Coalesce the tar writer's many small writes into ~64 KiB frames, so the body
/// is a handful of sizeable chunks rather than thousands of tiny ones.
const CHUNK_BYTES: usize = 64 * 1024;

/// Which Dockerfile the context ships: one synthesized from an inline string, or
/// a named file already inside the context directory.
pub(super) enum ContextSource {
	/// `build.dockerfile_inline` content.
	Inline(String),
	/// The resolved Dockerfile/Containerfile name within the context.
	Dockerfile(String),
}

/// A [`Write`] sink that forwards the tar bytes to an async channel as `Bytes`
/// frames, coalescing small writes to `CHUNK_BYTES`. Blocks (backpressure) when
/// the consumer is behind; errors if the consumer has gone away.
struct ChannelWriter {
	tx: mpsc::Sender<BodyItem>,
	buf: BytesMut,
}

impl ChannelWriter {
	fn send_pending(&mut self) -> io::Result<()> {
		if self.buf.is_empty() {
			return Ok(());
		}
		let chunk = self.buf.split().freeze();
		self.tx.blocking_send(Ok(Frame::data(chunk))).map_err(|_| {
			io::Error::new(
				io::ErrorKind::BrokenPipe,
				"build-context receiver dropped before the tar finished",
			)
		})
	}
}

impl Write for ChannelWriter {
	fn write(&mut self, data: &[u8]) -> io::Result<usize> {
		self.buf.extend_from_slice(data);
		if self.buf.len() >= CHUNK_BYTES {
			self.send_pending()?;
		}
		Ok(data.len())
	}

	fn flush(&mut self) -> io::Result<()> {
		self.send_pending()
	}
}

/// Spawn the blocking tar writer and return `(producer, body)`.
///
/// The producer's `JoinHandle` yields the context-assembly `Result` (a tar/IO
/// error surfaces here with its descriptive message); the `body` is the stream
/// the client sends as the request body. Await the body request first, then the
/// producer; awaiting the producer before draining the body would deadlock on
/// the bounded channel.
pub(super) fn context_body(
	context: PathBuf,
	source: ContextSource,
	secret_files: Vec<(String, Vec<u8>)>,
) -> (
	JoinHandle<Result<()>>,
	impl Stream<Item = BodyItem> + Send + 'static,
) {
	let (tx, rx) = mpsc::channel::<BodyItem>(CHANNEL_CAP);

	let producer = tokio::task::spawn_blocking(move || -> Result<()> {
		let mut writer = ChannelWriter {
			tx,
			buf: BytesMut::with_capacity(CHUNK_BYTES),
		};
		match source {
			ContextSource::Inline(inline) => {
				stream_build_context_with_inline(&mut writer, &context, &inline, &secret_files)
			}
			ContextSource::Dockerfile(dockerfile) => {
				stream_build_context(&mut writer, &context, &dockerfile, &secret_files)
			}
		}
	});

	// Turn the receiver into a body stream without pulling in `tokio-stream`:
	// `unfold` re-yields the receiver after each `recv`, ending on channel close.
	let body = futures_util::stream::unfold(rx, |mut rx| async move {
		rx.recv().await.map(|item| (item, rx))
	});

	(producer, body)
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
