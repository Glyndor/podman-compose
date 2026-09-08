//! Tests for `Client::post_hijack`. The pre-existing cases cover a 404,
//! a closed-by-peer connection, and an over-long head. #1747 (L3) adds two
//! more: a peer that accepts the handshake but never sends a byte, and a
//! peer that never accepts the connect at all. Both would otherwise wedge
//! `post_hijack` forever; the fix's wall-clock ceilings are what we pin
//! here. The test-local `tokio::time::pause()` keeps the wall-clock budget
//! small enough to run in a few hundred milliseconds.

use super::*;

/// A refused exec must surface as an error *before* the caller puts the
/// terminal into raw mode. Returning a live socket for a 404 would hang with
/// the shell already unusable.
#[tokio::test]
async fn a_non_2xx_head_is_an_error_not_a_stream() {
	let dir = tempfile::tempdir().unwrap();
	let sock = dir.path().join("s.sock");
	let listener = tokio::net::UnixListener::bind(&sock).unwrap();
	tokio::spawn(async move {
		if let Ok((mut c, _)) = listener.accept().await {
			use tokio::io::AsyncWriteExt;
			let _ = c
				.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
				.await;
		}
	});

	let client = Client::new(sock.to_string_lossy().to_string());
	let err = client
		.post_hijack("/exec/abc/start", b"{}")
		.await
		.expect_err("a 404 must not yield a stream");
	assert!(err.is_status(404), "got {err:?}");
}

/// A server that closes without answering is reported, not treated as a
/// successful attach.
#[tokio::test]
async fn a_closed_connection_is_an_error() {
	let dir = tempfile::tempdir().unwrap();
	let sock = dir.path().join("s.sock");
	let listener = tokio::net::UnixListener::bind(&sock).unwrap();
	tokio::spawn(async move {
		let _ = listener.accept().await;
	});

	let client = Client::new(sock.to_string_lossy().to_string());
	assert!(client.post_hijack("/exec/abc/start", b"{}").await.is_err());
}

/// A peer that never sends the head terminator is drained up to
/// [`MAX_HEAD_BYTES`] and reported, not just rejected at the first byte.
#[tokio::test]
async fn a_head_without_terminator_drains_then_errors() {
	let dir = tempfile::tempdir().unwrap();
	let sock = dir.path().join("s.sock");
	let listener = tokio::net::UnixListener::bind(&sock).unwrap();
	let payload = vec![b'A'; MAX_HEAD_BYTES];
	tokio::spawn(async move {
		if let Ok((mut c, _)) = listener.accept().await {
			use tokio::io::AsyncWriteExt;
			let _ = c.write_all(&payload).await;
		}
	});

	let client = Client::new(sock.to_string_lossy().to_string());
	let err = client
		.post_hijack("/exec/abc/start", b"{}")
		.await
		.expect_err("an endless head must not yield a stream");
	assert!(err.is_status(0), "got {err:?}");
	assert!(format!("{err}").contains("response head exceeded its limit"));
}
/// #1747 (L3): a peer that accepts the handshake then goes silent would
/// otherwise hang `read_response_head` forever. The fix wraps the head
/// read in `HEAD_READ_TIMEOUT` and surfaces a structured error with a
/// `timed out` message. Pausing the tokio clock makes the wall-clock
/// budget fire instantly inside the test, rather than waiting the
/// production 30 seconds.
#[tokio::test(start_paused = true)]
async fn a_silent_peer_surfaces_a_read_timeout() {
	let dir = tempfile::tempdir().unwrap();
	let sock = dir.path().join("s.sock");
	let listener = tokio::net::UnixListener::bind(&sock).unwrap();
	// Accept the connection and then hold it open without writing a single
	// byte to the response head. `read_response_head` would otherwise keep
	// reading forever; the fix's `HEAD_READ_TIMEOUT` bounds it.
	tokio::spawn(async move {
		while let Ok((mut c, _)) = listener.accept().await {
			// Hold the connection open by parking it in the runtime.
			// The fix's HEAD_READ_TIMEOUT will fire on the client side
			// before we ever write anything.
			tokio::spawn(async move {
				// Read the request (consume it so the client can finish
				// its write half).
				use tokio::io::AsyncReadExt;
				let mut buf = [0u8; 1024];
				let _ = c.read(&mut buf).await;
				// Hold indefinitely without responding. The client's
				// HEAD_READ_TIMEOUT kicks in first.
				tokio::time::sleep(std::time::Duration::from_secs(120)).await;
				let _ = c;
			});
		}
	});
	let client = Client::new(sock.to_string_lossy().to_string());
	let err = client
		.post_hijack("/exec/abc/start", b"{}")
		.await
		.expect_err("a silent peer must error, not hang");
	let msg = format!("{err}");
	assert!(
		msg.contains("timed out"),
		"the error should name the read timeout, got: {msg}"
	);
	assert!(err.is_status(0), "got {err:?}");
}
