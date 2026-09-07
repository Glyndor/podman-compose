// The pool's connection-reuse semantics ride on `UnixListener`, which is
// only available on Unix. The pool itself is cross-platform (Windows uses
// a named pipe; see `internal/libpod/client/stream.rs`); the integration
// test that proves keep-alive reuses a single socket is Unix-only by
// necessity. The `#[cfg(unix)]` here skips the test on Windows CI; the
// pool's other unit tests (which don't bind a socket) still run there.
#![cfg(unix)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

/// Fake libpod server that replies with `Transfer-Encoding: chunked` so the
/// body length is not declared up front in the headers. The pool's existing
/// harness (`CountingServer`) uses `Content-Length` and returns the whole
/// body at once, so its responses are never in flight when the guard is
/// released. That is a property of the harness, not of the framing: a
/// declared length large enough or delivered slowly enough leaves a body in
/// flight just as a chunked one does. What this fixture adds is a body that
/// is deliberately still arriving when `send` returns, which is the
/// condition the defect needs. Headers go out immediately so the client can
/// parse them and
/// `send` can return; the body follows after a short delay so concurrent
/// callers have time to race for the connection.
struct ChunkedServer {
	sock_path: std::path::PathBuf,
	accepted: Arc<AtomicUsize>,
	_dir: tempfile::TempDir,
	task: tokio::task::JoinHandle<()>,
}

impl ChunkedServer {
	async fn start() -> Self {
		let dir = tempfile::tempdir().unwrap();
		let sock_path = dir.path().join("podman.sock");
		let listener = UnixListener::bind(&sock_path).unwrap();
		let accepted = Arc::new(AtomicUsize::new(0));
		let accepted_clone = accepted.clone();

		let task = tokio::spawn(async move {
			loop {
				let Ok((mut stream, _)) = listener.accept().await else {
					break;
				};
				accepted_clone.fetch_add(1, Ordering::SeqCst);
				tokio::spawn(async move {
					loop {
						// Read one request.
						let mut buf = Vec::new();
						let mut chunk = [0u8; 1024];
						let mut got_request = false;
						while !got_request {
							match stream.read(&mut chunk).await {
								Ok(0) => return,
								Ok(n) => {
									buf.extend_from_slice(&chunk[..n]);
									if buf.windows(4).any(|w| w == b"\r\n\r\n") {
										got_request = true;
									}
								}
								Err(_) => return,
							}
						}
						// Headers go out immediately so `send` returns on the
						// client. The body delay is the window during which
						// the next caller can race for the connection.
						let headers = b"HTTP/1.1 200 OK\r\n\
							content-type: application/json\r\n\
							transfer-encoding: chunked\r\n\
							\r\n";
						if stream.write_all(headers).await.is_err() {
							return;
						}
						if stream.flush().await.is_err() {
							return;
						}
						tokio::time::sleep(std::time::Duration::from_millis(50)).await;
						// Two chunks, body is `{"ok":true}`.
						let body = b"{\"ok\":true}";
						let mid = body.len() / 2;
						for part in [&body[..mid], &body[mid..]] {
							let chunk = format!("{:x}\r\n", part.len());
							if stream.write_all(chunk.as_bytes()).await.is_err() {
								return;
							}
							if stream.write_all(part).await.is_err() {
								return;
							}
							if stream.write_all(b"\r\n").await.is_err() {
								return;
							}
							if stream.flush().await.is_err() {
								return;
							}
							tokio::time::sleep(std::time::Duration::from_millis(10)).await;
						}
						if stream.write_all(b"0\r\n\r\n").await.is_err() {
							return;
						}
						if stream.flush().await.is_err() {
							return;
						}
					}
				});
			}
		});

		Self {
			sock_path,
			accepted,
			_dir: dir,
			task,
		}
	}

	fn sock_str(&self) -> String {
		self.sock_path.to_string_lossy().into_owned()
	}

	fn accepted(&self) -> usize {
		self.accepted.load(Ordering::SeqCst)
	}
}

impl Drop for ChunkedServer {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// With chunked encoding and a body that is delayed after the headers, the
/// pool guard MUST stay held until the body is fully read. If it is released
/// between `send` and `read_body`, the next acquirer can write a new request
/// to the same socket while the first body is still arriving; the bytes
/// interleave and the JSON parser sees garbage (#1740). The existing harness
/// could not see this because it answered in one piece, so nothing was ever
/// in flight at the moment the guard was released.
///
/// Pool cap = 1 forces every concurrent caller through the same socket; the
/// race is the point. 8 tasks x 20 requests = 160 round-trips that contend
/// for the single tracked connection. Tasks that find it busy open a
/// transient connection (dropped on release); tasks that find it idle take
/// it. With the guard released before the body is drained, the second case
/// is what corrupts the first task's still-arriving body (#1740).
#[tokio::test]
async fn pool_reuse_does_not_corrupt_chunked_responses() {
	let server = ChunkedServer::start().await;
	let client = std::sync::Arc::new(crate::libpod::Client::with_pool_size(server.sock_str(), 1));

	let mut handles = Vec::with_capacity(8);
	for _ in 0..8 {
		let c = client.clone();
		handles.push(tokio::spawn(async move {
			for _ in 0..20 {
				let resp: serde_json::Value =
					c.get_json("/libpod/_ping").await.unwrap_or_else(|e| {
						// Not labelled "corrupted": this arm catches ANY
						// error, and a hyper dispatch cancellation
						// (`Canceled, "connection was not ready"`, #1758)
						// arrives here too. Naming the cause in the message
						// would make the test a source of false diagnosis,
						// which is what the previous wording did.
						panic!("request failed against the chunked fixture: {e}")
					});
				assert_eq!(
					resp,
					serde_json::json!({"ok": true}),
					"chunked body corrupted"
				);
			}
		}));
	}
	for h in handles {
		h.await.unwrap();
	}
	// Reuse, not merely "a connection happened". 160 round-trips through a
	// cap-of-one pool must not open 160 sockets; a client that never reused
	// anything would. The previous assertion here was `accepted() >= 1`,
	// which a client opening a fresh connection per call also satisfies, so
	// it did not hold down the property its comment claimed.
	//
	// The exact count is non-deterministic: a task that finds the single
	// tracked connection busy opens a transient one, and the listener counts
	// each separately. The bound is what is deterministic: strictly fewer
	// sockets than requests means at least one was reused.
	let accepted = server.accepted();
	assert!(
		(1..160).contains(&accepted),
		"the pool must reuse: {accepted} sockets accepted for 160 requests"
	);
}
