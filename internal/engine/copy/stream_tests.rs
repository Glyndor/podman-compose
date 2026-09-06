use super::*;

/// Feed a reader from a list of chunks, as the body pump would.
fn reader_over(chunks: Vec<ChunkItem>, cap: u64) -> ChannelReader {
	let (tx, rx) = mpsc::channel(CHANNEL_CAP);
	for c in chunks {
		tx.try_send(c)
			.expect("test channel must accept the fixture");
	}
	drop(tx);
	ChannelReader::new(rx, cap)
}

#[test]
fn chunks_are_reassembled_in_order() {
	let mut r = reader_over(
		vec![
			Ok(Bytes::from_static(b"hello ")),
			Ok(Bytes::from_static(b"world")),
		],
		1024,
	);
	let mut out = String::new();
	r.read_to_string(&mut out).expect("read");
	assert_eq!(out, "hello world");
}

/// The reader must survive a producer that sends an empty frame. Reporting
/// end-of-stream there would truncate the archive with no error anywhere,
/// exactly the silent-data-loss shape this file exists to avoid.
#[test]
fn an_empty_frame_is_not_end_of_stream() {
	let mut r = reader_over(
		vec![
			Ok(Bytes::from_static(b"a")),
			Ok(Bytes::new()),
			Ok(Bytes::from_static(b"b")),
		],
		1024,
	);
	let mut out = Vec::new();
	r.read_to_end(&mut out).expect("read");
	assert_eq!(out, b"ab", "the empty frame must be skipped, not terminate");
}

/// A short buffer must not lose the rest of the chunk.
#[test]
fn a_chunk_larger_than_the_buffer_is_served_across_reads() {
	let mut r = reader_over(vec![Ok(Bytes::from_static(b"abcdef"))], 1024);
	let mut buf = [0u8; 2];
	let mut seen = Vec::new();
	loop {
		let n = r.read(&mut buf).expect("read");
		if n == 0 {
			break;
		}
		seen.extend_from_slice(&buf[..n]);
	}
	assert_eq!(seen, b"abcdef");
}

/// The cap is what stops a compromised container streaming without end onto the
/// host's disk. Assert the specific refusal rather than any error: a bare
/// `is_err` would be satisfied by a channel fault too.
#[test]
fn past_the_cap_the_read_is_refused_for_being_over_the_cap() {
	let mut r = reader_over(vec![Ok(Bytes::from_static(b"0123456789"))], 4);
	let mut out = Vec::new();
	let err = r.read_to_end(&mut out).expect_err("must refuse");
	assert!(
		err.to_string().contains("exceeds 4 bytes"),
		"must name the cap it exceeded, got: {err}"
	);
}

/// The acceptance half of the pair above, one byte inside the limit. Without
/// it, an archive refused for an unrelated reason would satisfy the rejection
/// test and prove nothing about the cap.
#[test]
fn exactly_at_the_cap_is_served() {
	let mut r = reader_over(vec![Ok(Bytes::from_static(b"0123"))], 4);
	let mut out = Vec::new();
	r.read_to_end(&mut out)
		.expect("four bytes under a cap of four must pass");
	assert_eq!(out, b"0123");
}

/// A transport failure mid-stream must reach the extractor as an error, not as
/// a clean end of file. The distinction is the whole point: a truncated archive
/// that ends cleanly is silent data loss.
#[test]
fn a_transport_error_is_not_a_clean_end() {
	let mut r = reader_over(
		vec![
			Ok(Bytes::from_static(b"partial")),
			Err(io::Error::other("connection reset")),
		],
		1024,
	);
	let mut buf = [0u8; 64];
	assert_eq!(r.read(&mut buf).expect("first chunk"), 7);
	let err = r.read(&mut buf).expect_err("the fault must surface");
	assert!(
		err.to_string().contains("connection reset"),
		"the transport cause must survive, got: {err}"
	);
}
