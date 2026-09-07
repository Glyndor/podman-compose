// `mkfifo` is the only way to plant a FIFO node; the call is a single libc
// FFI, it operates on the supplied path, and it never reads from the FIFO
// itself. The opt-out is scoped to this test file.
#![allow(unsafe_code)]

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
/// #1747 (L2): an `env_file:` (or any compose-side path) that points at a
/// FIFO with no writer used to wedge the parser: `File::open` blocked in
/// the kernel, no message, no timeout. Refuse up front with an actionable
/// error so a typo in compose.yml is a 1-second failure, not a hang. The
/// reader never opens the FIFO, so the test does not need a writer.
#[cfg(unix)]
#[test]
fn read_to_string_capped_rejects_a_fifo() {
	use std::ffi::CString;
	use std::os::unix::ffi::OsStrExt;
	let dir = tempfile::tempdir().expect("tempdir");
	let fifo = dir.path().join("env.fifo");
	// Plant the FIFO via libc::mkfifo (no `nix` dependency); the goal is
	// to land the node on disk without ever opening the read end, which is
	// what wedged the unfixed code.
	let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("cstring");
	let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
	assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
	let err = read_to_string_capped_with(&fifo, 64).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
	let msg = err.to_string();
	assert!(
		msg.contains("FIFO"),
		"the kind label should be in the message, got: {msg}"
	);
	assert!(
		msg.contains("would block"),
		"the error should explain the hang, got: {msg}"
	);
}
#[cfg(unix)]
#[test]
fn read_capped_rejects_a_fifo() {
	// Same refused-input shape from the bytes reader (build secrets).
	use std::ffi::CString;
	use std::os::unix::ffi::OsStrExt;
	let dir = tempfile::tempdir().expect("tempdir");
	let fifo = dir.path().join("env.fifo");
	let c_path = CString::new(fifo.as_os_str().as_bytes()).expect("cstring");
	let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
	assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());
	let err = read_capped_with(&fifo, 64).unwrap_err();
	assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
	let msg = err.to_string();
	assert!(
		msg.contains("FIFO"),
		"the kind label should be in the message, got: {msg}"
	);
	assert!(
		msg.contains("would block"),
		"the error should explain the hang, got: {msg}"
	);
}
