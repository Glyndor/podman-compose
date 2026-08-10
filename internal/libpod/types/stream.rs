//! Multiplexed log/exec stream parser.
//!
//! Docker and Podman use an 8-byte frame header before each payload chunk:
//! `[stream_type: u8][0][0][0][size_big_endian: u32][payload]`
//! Stream type 1 = stdout, 2 = stderr.

use bytes::{Bytes, BytesMut};
use futures_util::stream::Stream;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use std::pin::Pin;

use crate::libpod::error::PodmanError;

/// A single framed chunk from a multiplexed container log or exec stream.
#[derive(Debug)]
pub enum LogOutput {
	/// Payload demuxed from the stdout stream (frame stream type 1).
	StdOut { message: Bytes },
	/// Payload demuxed from the stderr stream (frame stream type 2).
	StdErr { message: Bytes },
}

/// Boxed stream alias used for parse_multiplexed and parse_json_lines return types.
pub type BoxStream<T> = Pin<Box<dyn Stream<Item = Result<T, PodmanError>> + Send>>;

/// Upper bound on one multiplexed frame or buffered NDJSON record. This matches
/// moby's maximum frame size and limits daemon-controlled allocations.
pub const MAX_STREAM_BUF: usize = 1024 * 1024;

/// Add received bytes to a stream's current buffered-byte count.
///
/// Returns [`PodmanError::StreamTooLarge`] without changing the count when the
/// addition would exceed [`MAX_STREAM_BUF`]. Call this before extending the
/// corresponding [`BytesMut`] so rejected input cannot trigger the allocation.
pub fn record_stream_bytes(total_received: &mut u64, received: usize) -> Result<(), PodmanError> {
	record_buffered_bytes(total_received, received, MAX_STREAM_BUF)
}

fn record_buffered_bytes(
	total_received: &mut u64,
	received: usize,
	limit: usize,
) -> Result<(), PodmanError> {
	let next = total_received
		.checked_add(received as u64)
		.ok_or(PodmanError::StreamTooLarge)?;
	if next > limit as u64 {
		return Err(PodmanError::StreamTooLarge);
	}
	*total_received = next;
	Ok(())
}

// ---------------------------------------------------------------------------
// Pure parsing helpers (also used by unit tests)
// ---------------------------------------------------------------------------

/// Try to consume one complete multiplexed frame from the front of `buf`.
///
/// The wire header is 8 bytes: 4 bytes of stream metadata followed by a
/// big-endian `u32` payload size. On success the header and payload are split
/// off the front of `buf` (the remaining bytes stay buffered for the next
/// frame) and `Some((stream_type, payload))` is returned. The payload is a
/// zero-copy [`Bytes`] sharing the original allocation, so no per-frame copy or
/// tail memmove occurs. Returns `Ok(None)` (leaving `buf` untouched) when fewer
/// than a full frame is buffered and more data is needed. Returns
/// [`PodmanError::StreamTooLarge`] before splitting when the announced payload
/// exceeds [`MAX_STREAM_BUF`].
pub fn parse_frame(buf: &mut BytesMut) -> Result<Option<(u8, Bytes)>, PodmanError> {
	if buf.len() < 8 {
		return Ok(None);
	}
	let size = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
	if size > MAX_STREAM_BUF {
		return Err(PodmanError::StreamTooLarge);
	}
	let frame_len = 8 + size;
	if buf.len() < frame_len {
		return Ok(None);
	}
	let stream_type = buf[0];
	let mut frame = buf.split_to(frame_len);
	let payload = frame.split_off(8).freeze();
	Ok(Some((stream_type, payload)))
}

/// Pop the next newline-terminated line from the front of `buf`, excluding the
/// newline byte.
///
/// On success the line and its trailing `\n` are split off the front of `buf`
/// in O(1) (no tail memmove) and the line is returned as a zero-copy [`Bytes`]
/// sharing the original allocation. Returns `None` (leaving `buf` untouched)
/// when no complete line is buffered yet.
pub fn take_json_line(buf: &mut BytesMut) -> Option<Bytes> {
	let nl = buf.iter().position(|&b| b == b'\n')?;
	let mut line = buf.split_to(nl + 1);
	line.truncate(nl); // drop the trailing newline byte
	Some(line.freeze())
}

// ---------------------------------------------------------------------------
// Async stream parsers
// ---------------------------------------------------------------------------

/// Parse a multiplexed stream from a hyper `Incoming` response body.
///
/// Emits [`LogOutput`] items as frames arrive. The returned stream ends when
/// the response body is fully consumed.
pub fn parse_multiplexed(body: Incoming) -> BoxStream<LogOutput> {
	Box::pin(futures_util::stream::try_unfold(
		(body, BytesMut::new(), 0u64),
		|(mut body, mut buf, mut total_received)| async move {
			loop {
				if let Some((stream_type, payload)) = parse_frame(&mut buf)? {
					let output = match stream_type {
						1 => LogOutput::StdOut { message: payload },
						2 => LogOutput::StdErr { message: payload },
						_ => continue,
					};
					return Ok(Some((output, (body, buf, total_received))));
				}

				match body.frame().await {
					Some(Ok(frame)) => {
						if let Ok(data) = frame.into_data() {
							// The frame header can sit ahead of the payload, so
							// permit a small lead-in past the per-frame cap.
							record_buffered_bytes(
								&mut total_received,
								data.len(),
								MAX_STREAM_BUF + 8,
							)?;
							buf.extend_from_slice(&data);
						}
					}
					Some(Err(e)) => return Err(PodmanError::from(e)),
					None => return Ok(None),
				}
			}
		},
	))
}

/// Parse a raw (non-multiplexed) stream from a hyper `Incoming` response body.
///
/// Used for TTY containers where Podman sends raw bytes without 8-byte frame
/// headers. All bytes are treated as stdout since TTY merges the streams.
pub fn parse_raw(body: Incoming) -> BoxStream<LogOutput> {
	Box::pin(futures_util::stream::try_unfold(
		body,
		|mut body| async move {
			loop {
				match body.frame().await {
					Some(Ok(frame)) => {
						if let Ok(data) = frame.into_data() {
							if !data.is_empty() {
								return Ok(Some((LogOutput::StdOut { message: data }, body)));
							}
						}
					}
					Some(Err(e)) => return Err(PodmanError::from(e)),
					None => return Ok(None),
				}
			}
		},
	))
}

/// Parse a newline-delimited JSON stream (used for image pull and build output).
///
/// Each line in the stream is expected to be a complete JSON object. Blank
/// lines between objects are silently skipped.
pub fn parse_json_lines<T: serde::de::DeserializeOwned + Send + 'static>(
	body: Incoming,
) -> BoxStream<T> {
	Box::pin(futures_util::stream::try_unfold(
		(body, BytesMut::new(), 0u64),
		|(mut body, mut buf, mut total_received)| async move {
			loop {
				if let Some(line) = take_json_line(&mut buf) {
					// A line plus its trailing newline are no longer buffered
					// once the line is parsed; account for the freed space so
					// the cumulative counter reflects what the daemon still
					// owes the parser, not the bytes the parser has already
					// consumed.
					total_received = total_received.saturating_sub((line.len() + 1) as u64);
					if line.is_empty() {
						continue;
					}
					let item: T = serde_json::from_slice(&line).map_err(PodmanError::Json)?;
					return Ok(Some((item, (body, buf, total_received))));
				}

				match body.frame().await {
					Some(Ok(frame)) => {
						if let Ok(data) = frame.into_data() {
							record_stream_bytes(&mut total_received, data.len())?;
							buf.extend_from_slice(&data);
						}
					}
					Some(Err(e)) => return Err(PodmanError::from(e)),
					None if buf.is_empty() => return Ok(None),
					None => {
						// Trailing bytes with no terminating newline: a complete
						// record still parses, and a truncated one is the
						// "stream ended early" case the issue calls out, not a
						// serde error whose cause is the daemon's cut.
						let line = std::mem::take(&mut buf);
						total_received = 0;
						let item: T = serde_json::from_slice(&line)
							.map_err(|_| PodmanError::StreamEndedEarly)?;
						return Ok(Some((item, (body, buf, total_received))));
					}
				}
			}
		},
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	// ---------------------------------------------------------------------------
	// parse_frame tests
	// ---------------------------------------------------------------------------

	#[test]
	fn parse_frame_rejects_oversized_payload_before_split() {
		let mut buf = BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff][..]);
		assert!(matches!(
			parse_frame(&mut buf),
			Err(PodmanError::StreamTooLarge)
		));
		assert_eq!(buf.len(), 8);
	}

	#[test]
	fn parse_frame_incomplete_header() {
		let mut buf = BytesMut::from(&[0x01, 0x00, 0x00, 0x00][..]);
		assert!(parse_frame(&mut buf).unwrap().is_none());
		// A `None` result must leave the buffer untouched.
		assert_eq!(buf.as_ref(), &[0x01, 0x00, 0x00, 0x00]);
	}

	#[test]
	fn parse_frame_header_present_payload_missing() {
		// Header says 5-byte payload but buffer only has 3.
		let mut buf = BytesMut::from(
			&[
				0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, b'a', b'b', b'c',
			][..],
		);
		assert!(parse_frame(&mut buf).unwrap().is_none());
		// Partial frame stays buffered for the next read.
		assert_eq!(buf.len(), 11);
	}

	#[test]
	fn parse_frame_stdout_complete() {
		let mut buf = BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05][..]);
		buf.extend_from_slice(b"hello");
		let (stype, data) = parse_frame(&mut buf).unwrap().unwrap();
		assert_eq!(stype, 1);
		assert_eq!(data.as_ref(), b"hello");
		// The full frame is consumed from the front.
		assert!(buf.is_empty());
	}

	#[test]
	fn parse_frame_stderr_complete() {
		let mut buf = BytesMut::from(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03][..]);
		buf.extend_from_slice(b"err");
		let (stype, data) = parse_frame(&mut buf).unwrap().unwrap();
		assert_eq!(stype, 2);
		assert_eq!(data.as_ref(), b"err");
		assert!(buf.is_empty());
	}

	#[test]
	fn parse_frame_zero_length_payload() {
		let mut buf = BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00][..]);
		let (stype, data) = parse_frame(&mut buf).unwrap().unwrap();
		assert_eq!(stype, 1);
		assert!(data.is_empty());
		assert!(buf.is_empty());
	}

	#[test]
	fn parse_frame_leaves_remainder() {
		// Buffer has one full frame + extra bytes.
		let mut buf =
			BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, b'h', b'i'][..]);
		buf.extend_from_slice(b"leftover");
		let (_, data) = parse_frame(&mut buf).unwrap().unwrap();
		assert_eq!(data.as_ref(), b"hi");
		// Only the consumed frame is removed; the remainder is left in place.
		assert_eq!(buf.as_ref(), b"leftover");
	}

	#[test]
	fn parse_frame_two_frames_in_one_buffer_demux() {
		// One stdout frame ("hi") immediately followed by one stderr frame
		// ("er") must demux to the correct stream, in order.
		let mut buf =
			BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, b'h', b'i'][..]);
		buf.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, b'e', b'r']);
		let (stype1, data1) = parse_frame(&mut buf).unwrap().unwrap();
		assert_eq!(stype1, 1);
		assert_eq!(data1.as_ref(), b"hi");
		let (stype2, data2) = parse_frame(&mut buf).unwrap().unwrap();
		assert_eq!(stype2, 2);
		assert_eq!(data2.as_ref(), b"er");
		assert!(buf.is_empty());
		assert!(parse_frame(&mut buf).unwrap().is_none());
	}

	#[test]
	fn parse_frame_split_across_reads_reassembles() {
		// First read delivers only the header plus a partial payload; the frame
		// must not parse until the rest arrives in a second read.
		let mut buf = BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05][..]);
		buf.extend_from_slice(b"hel");
		assert!(parse_frame(&mut buf).unwrap().is_none());
		// Second read completes the payload.
		buf.extend_from_slice(b"lo");
		let (stype, data) = parse_frame(&mut buf).unwrap().unwrap();
		assert_eq!(stype, 1);
		assert_eq!(data.as_ref(), b"hello");
		assert!(buf.is_empty());
	}

	// ---------------------------------------------------------------------------
	// take_json_line tests
	// ---------------------------------------------------------------------------

	#[test]
	fn take_json_line_no_newline() {
		let mut buf = BytesMut::from(&b"partial line"[..]);
		assert!(take_json_line(&mut buf).is_none());
		assert_eq!(buf.as_ref(), b"partial line");
	}

	#[test]
	fn take_json_line_with_newline() {
		let mut buf = BytesMut::from(&b"line1\nline2"[..]);
		let line = take_json_line(&mut buf).unwrap();
		assert_eq!(line.as_ref(), b"line1");
		assert_eq!(buf.as_ref(), b"line2");
	}

	#[test]
	fn take_json_line_empty_line() {
		let mut buf = BytesMut::from(&b"\nnext"[..]);
		let line = take_json_line(&mut buf).unwrap();
		assert!(line.is_empty());
		assert_eq!(buf.as_ref(), b"next");
	}

	#[test]
	fn take_json_line_multiple_lines() {
		let mut buf = BytesMut::from(&b"a\nb\nc"[..]);
		assert_eq!(take_json_line(&mut buf).unwrap().as_ref(), b"a");
		assert_eq!(take_json_line(&mut buf).unwrap().as_ref(), b"b");
		assert!(take_json_line(&mut buf).is_none());
	}

	#[test]
	fn take_json_line_multiple_lines_in_one_buffer_in_order() {
		// Several complete JSON lines delivered in a single buffer fill must be
		// returned one at a time, in arrival order, with the remainder kept.
		let mut buf = BytesMut::from(
			&br#"{"a":1}
{"b":2}
{"c":3}
"#[..],
		);
		assert_eq!(take_json_line(&mut buf).unwrap().as_ref(), br#"{"a":1}"#);
		assert_eq!(take_json_line(&mut buf).unwrap().as_ref(), br#"{"b":2}"#);
		assert_eq!(take_json_line(&mut buf).unwrap().as_ref(), br#"{"c":3}"#);
		assert!(take_json_line(&mut buf).is_none());
		assert!(buf.is_empty());
	}

	#[test]
	fn take_json_line_split_across_reads_reassembles() {
		// A line whose newline only arrives in the second read must not be
		// returned until that read completes it.
		let mut buf = BytesMut::from(&b"{\"a\":"[..]);
		assert!(take_json_line(&mut buf).is_none());
		buf.extend_from_slice(b"1}\n");
		assert_eq!(take_json_line(&mut buf).unwrap().as_ref(), br#"{"a":1}"#);
		assert!(buf.is_empty());
	}

	// ---------------------------------------------------------------------------
	// MAX_STREAM_BUF cap
	// ---------------------------------------------------------------------------

	/// Mirror of the cap check the async parsers run after each buffer fill
	/// (`buf.len() > MAX_STREAM_BUF`). Returns the overflow error when, and only
	/// when, the reassembly buffer has grown strictly past the limit.
	fn cap_check(buf_len: usize) -> Option<PodmanError> {
		let mut total_received = 0;
		record_stream_bytes(&mut total_received, buf_len).err()
	}

	#[test]
	fn cumulative_buffered_bytes_are_rejected_before_extend() {
		let mut total_received = 0;
		record_stream_bytes(&mut total_received, MAX_STREAM_BUF - 1).unwrap();
		assert!(matches!(
			record_stream_bytes(&mut total_received, 2),
			Err(PodmanError::StreamTooLarge)
		));
		assert_eq!(total_received, (MAX_STREAM_BUF - 1) as u64);
	}

	#[test]
	fn over_cap_buffer_is_rejected() {
		// A buffer that grows one byte past the cap must trip the overflow guard
		// with the documented StreamTooLarge variant.
		assert!(matches!(
			cap_check(MAX_STREAM_BUF + 1),
			Some(PodmanError::StreamTooLarge)
		));
	}

	#[test]
	fn at_cap_buffer_is_accepted() {
		// A buffer exactly at the cap must be allowed; the guard rejects only a
		// buffer strictly greater than MAX_STREAM_BUF.
		assert!(cap_check(MAX_STREAM_BUF).is_none());
	}
}
