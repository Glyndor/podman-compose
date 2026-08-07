#![no_main]

//! Fuzz the libpod stream framer with raw daemon-controlled bytes: the 8-byte
//! multiplexed frame header (including hostile size fields) and the
//! newline-delimited JSON line splitter. Must never panic or index out of
//! bounds on malformed framing.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let mut frame_buf = BytesMut::from(data);
	loop {
		match podup::fuzz_api::parse_frame(&mut frame_buf) {
			Ok(Some(_)) => {}
			Ok(None) => break,
			Err(podup::fuzz_api::PodmanError::StreamTooLarge) => break,
			Err(error) => panic!("unexpected stream parser error: {error:?}"),
		}
	}

	let mut oversized_header = BytesMut::from(&[
		0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff,
	][..]);
	assert!(matches!(
		podup::fuzz_api::parse_frame(&mut oversized_header),
		Err(podup::fuzz_api::PodmanError::StreamTooLarge)
	));

	let mut buf = BytesMut::from(data);
	while podup::fuzz_api::take_json_line(&mut buf).is_some() {}
});
