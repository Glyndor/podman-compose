#![no_main]

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
	let mut total_received = 0;
	for chunk in data.chunks(1.max(data.len() / 64)) {
		if let Err(error) = podup::fuzz_api::record_stream_bytes(&mut total_received, chunk.len()) {
			assert!(matches!(
				error,
				podup::fuzz_api::PodmanError::StreamTooLarge
			));
			break;
		}
	}

	let mut cumulative = podup::fuzz_api::MAX_STREAM_BUF as u64 - 1;
	assert!(podup::fuzz_api::record_stream_bytes(&mut cumulative, 1).is_ok());
	assert!(matches!(
		podup::fuzz_api::record_stream_bytes(&mut cumulative, 1),
		Err(podup::fuzz_api::PodmanError::StreamTooLarge)
	));

	let mut buf = BytesMut::from(data);
	while podup::fuzz_api::take_json_line(&mut buf).is_some() {}
});
