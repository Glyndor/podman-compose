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
	let mut buf = BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, b'h', b'i'][..]);
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
	let mut buf = BytesMut::from(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, b'h', b'i'][..]);
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
