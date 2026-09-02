use super::{read_capped_from, read_capped_with, read_to_string_capped_with};

#[test]
fn read_capped_from_reads_within_limit() {
	// The shared reader (used for both files and stdin) returns the content
	// untouched when it fits under the cap.
	let out = read_capped_from(std::io::Cursor::new(b"version: 1"), 64, "stdin").unwrap();
	assert_eq!(out, "version: 1");
}

#[test]
fn read_capped_from_rejects_over_limit() {
	let err = read_capped_from(std::io::Cursor::new(vec![b'x'; 32]), 16, "stdin").unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
	assert!(err.to_string().contains("stdin"));
}

#[test]
fn reads_file_within_limit() {
	let dir = tempfile::tempdir().expect("tempdir");
	let f = dir.path().join("ok");
	std::fs::write(&f, b"hello").expect("write");
	assert_eq!(read_to_string_capped_with(&f, 16).unwrap(), "hello");
}

#[test]
fn rejects_file_over_limit() {
	let dir = tempfile::tempdir().expect("tempdir");
	let f = dir.path().join("big");
	std::fs::write(&f, vec![b'x'; 32]).expect("write");
	let err = read_to_string_capped_with(&f, 16).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn missing_file_is_error() {
	assert!(read_to_string_capped_with(std::path::Path::new("/no/such/file"), 16).is_err());
}

#[test]
fn read_capped_reads_bytes_within_limit() {
	let dir = tempfile::tempdir().expect("tempdir");
	let f = dir.path().join("ok");
	// Non-UTF-8 bytes must round-trip: the bytes reader does no UTF-8 check.
	std::fs::write(&f, [0xff, 0x00, 0xfe]).expect("write");
	assert_eq!(read_capped_with(&f, 16).unwrap(), vec![0xff, 0x00, 0xfe]);
}

#[test]
fn read_capped_rejects_bytes_over_limit() {
	let dir = tempfile::tempdir().expect("tempdir");
	let f = dir.path().join("big");
	std::fs::write(&f, vec![b'x'; 32]).expect("write");
	let err = read_capped_with(&f, 16).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn read_capped_missing_file_is_error() {
	assert!(read_capped_with(std::path::Path::new("/no/such/file"), 16).is_err());
}
