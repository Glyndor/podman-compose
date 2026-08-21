//! Bridge the container→host `cp` response body into a blocking tar reader
//! without buffering the whole archive.
//!
//! The mirror of `build/stream.rs`, which runs a blocking tar *writer* on a
//! `spawn_blocking` thread feeding a bounded channel that an async body drains.
//! Here the flow reverses: an async task drains the response body into the same
//! kind of bounded channel, and a blocking [`Read`] implementation pulls from
//! it so the tar extractor can work entry by entry.
//!
//! Peak memory is about `CHANNEL_CAP` chunks regardless of archive size, in
//! place of the whole archive.

use std::io::{self, Read};

use http_body_util::BodyExt;

use bytes::Bytes;
use tokio::sync::mpsc;

use super::archive::extract_tar_guarded;
use crate::error::{ComposeError, Result};

/// How many chunks the channel holds. Same reasoning as the build side: enough
/// to keep the socket and the extractor both busy, small enough that the bound
/// is the point.
pub(super) const CHANNEL_CAP: usize = 8;

/// A chunk of archive bytes, or the transport failure that ended the stream.
pub(super) type ChunkItem = io::Result<Bytes>;

/// Serves the bytes an async producer sends over a bounded channel to a
/// blocking consumer, refusing to serve more than `cap` in total.
///
/// The cap is no longer a memory bound — nothing accumulates — but it stays
/// meaningful: a compromised container can stream without end, and the bytes
/// land on the host's disk as they arrive. What it protects changed from RAM to
/// disk, so it is not redundant.
pub(super) struct ChannelReader {
	rx: mpsc::Receiver<ChunkItem>,
	/// The chunk currently being handed out, consumed from the front.
	current: Bytes,
	/// Bytes served so far, compared against `cap` on every read.
	served: u64,
	cap: u64,
}

impl ChannelReader {
	pub(super) fn new(rx: mpsc::Receiver<ChunkItem>, cap: u64) -> Self {
		Self {
			rx,
			current: Bytes::new(),
			served: 0,
			cap,
		}
	}
}

impl Read for ChannelReader {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		if buf.is_empty() {
			return Ok(0);
		}
		// A producer may send an empty frame; skip those rather than reporting
		// end of stream, which would truncate the archive silently.
		while self.current.is_empty() {
			match self.rx.blocking_recv() {
				// Channel closed: the producer finished or was dropped. Either
				// way there are no more bytes, which is a clean end of file —
				// a truncated archive surfaces as a tar error, not here.
				None => return Ok(0),
				Some(Err(e)) => return Err(e),
				Some(Ok(chunk)) => self.current = chunk,
			}
		}

		let n = buf.len().min(self.current.len());
		self.served += n as u64;
		if self.served > self.cap {
			return Err(io::Error::other(format!(
				"cp: container archive exceeds {} bytes",
				self.cap
			)));
		}
		buf[..n].copy_from_slice(&self.current[..n]);
		self.current = self.current.slice(n..);
		Ok(n)
	}
}

/// Pipe the archive body straight into the guarded extractor.
///
/// An async task drains the response body into a bounded channel; a
/// `spawn_blocking` task drives the tar extractor from the other end
/// through [`ChannelReader`]. Peak memory is the channel's depth rather
/// than the archive's size.
///
/// The pump is spawned before the extractor is awaited, and the extractor
/// is what the caller waits on. Awaiting the pump first would deadlock: it
/// blocks once the bounded channel fills, and only the extractor drains it.
/// This is the same ordering `build/stream.rs` documents in the other
/// direction.
pub(super) async fn extract_streamed(
	resp: hyper::Response<hyper::body::Incoming>,
	dst: std::path::PathBuf,
	cap: u64,
) -> Result<()> {
	let (tx, rx) = tokio::sync::mpsc::channel::<ChunkItem>(CHANNEL_CAP);

	let pump = tokio::spawn(async move {
		let mut body = resp.into_body();
		while let Some(frame) = body.frame().await {
			let item = match frame {
				Ok(f) => match f.into_data() {
					Ok(data) => Ok(data),
					// A trailers frame carries no payload and is not an
					// error; skip it rather than ending the archive.
					Err(_) => continue,
				},
				Err(e) => Err(std::io::Error::other(e.to_string())),
			};
			let fatal = item.is_err();
			// A send failure means the extractor is gone, which it does on
			// its own error. Stop rather than reporting a second fault over
			// the first.
			if tx.send(item).await.is_err() || fatal {
				break;
			}
		}
	});

	let reader = ChannelReader::new(rx, cap);
	let extracted = tokio::task::spawn_blocking(move || extract_tar_guarded(reader, &dst))
		.await
		.map_err(|e| ComposeError::Build(e.to_string()))?;

	// The extractor's verdict is the one that matters; the pump only
	// carries bytes. Abort it so a body still arriving after a refused
	// entry does not outlive the command.
	pump.abort();
	extracted
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
