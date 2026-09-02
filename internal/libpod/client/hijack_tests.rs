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
