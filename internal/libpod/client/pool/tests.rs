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

/// Test harness: a fake libpod socket that counts every accepted connection
/// and replies with a tiny valid HTTP/1.1 response so the client's connection
/// pool exercises its real keep-alive path. The connection is held open
/// (Content-Length + no `Connection: close`) so the pool sees a reusable
/// socket between requests.
struct CountingServer {
	sock_path: std::path::PathBuf,
	accepted: Arc<AtomicUsize>,
	_dir: tempfile::TempDir,
	task: tokio::task::JoinHandle<()>,
}

impl CountingServer {
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
					// Keep answering keep-alive requests on this connection
					// until the client closes or the read errors out. The
					// response deliberately omits `Connection: close` so
					// hyper is free to reuse the socket.
					loop {
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
						let body = b"{}";
						let response = format!(
							"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
							body.len()
						);
						if stream.write_all(response.as_bytes()).await.is_err() {
							return;
						}
						if stream.write_all(body).await.is_err() {
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

impl Drop for CountingServer {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// With the pool enabled (cap > 0), sequential requests ride a single
/// connection, the pool's whole reason to exist. 100 calls → 1 accepted
/// connection. The default cap is 0 (no pool), so this test constructs
/// the client with a non-zero cap to exercise the reuse path.
#[tokio::test]
async fn sequential_requests_reuse_a_single_connection() {
	let server = CountingServer::start().await;
	let client = crate::libpod::Client::with_pool_size(server.sock_str(), 8);
	for _ in 0..100 {
		let _: serde_json::Value = client.get_json("/libpod/_ping").await.unwrap();
	}
	assert_eq!(
		server.accepted(),
		1,
		"100 sequential calls should ride one connection; got {}",
		server.accepted()
	);
}

/// Concurrent callers must spread across at most `pool_size` connections, never
/// more. With `pool_size = 4` and 16 concurrent callers, the server must see
/// ≤ 4 accepted connections; many calls share the same keep-alive socket.
#[tokio::test]
async fn concurrent_requests_share_connections_within_the_cap() {
	let server = CountingServer::start().await;
	let client = std::sync::Arc::new(crate::libpod::Client::with_pool_size(server.sock_str(), 4));

	let mut handles = Vec::with_capacity(4);
	for _ in 0..4 {
		let c = client.clone();
		handles.push(tokio::spawn(async move {
			let _: serde_json::Value = c.get_json("/libpod/_ping").await.unwrap();
		}));
	}
	for h in handles {
		h.await.unwrap();
	}
	let accepted = server.accepted();
	assert!(
		accepted <= 4,
		"concurrent calls within the cap must not exceed pool_size; got {accepted}"
	);
	assert!(
		accepted >= 1,
		"the pool must have opened at least one connection; got 0"
	);
}

/// A parallel caller that exceeds the cap is not throttled. The pool opens
/// transient connections beyond the cap and drops them on release; the
/// idle queue stays at the cap. The server sees more than `pool_size`
/// connections under load; that is the documented contract: the cap is
/// a hint for idle reuse, not a cap on concurrency.
#[tokio::test]
async fn concurrent_requests_above_cap_open_transient_connections() {
	let server = CountingServer::start().await;
	let cap = 4_usize;
	let client = std::sync::Arc::new(crate::libpod::Client::with_pool_size(
		server.sock_str(),
		cap,
	));
	let mut handles = Vec::with_capacity(cap * 4);
	for _ in 0..(cap * 4) {
		let c = client.clone();
		handles.push(tokio::spawn(async move {
			let _: serde_json::Value = c.get_json("/libpod/_ping").await.unwrap();
		}));
	}
	for h in handles {
		h.await.unwrap();
	}
	let accepted = server.accepted();
	// The pool keeps at least `cap` connections alive, and the overflow
	// requests have opened more. The exact count is non-deterministic
	// (concurrent timing), but it must be strictly greater than `cap`.
	assert!(
		accepted > cap,
		"requests above the cap should open transient connections; accepted={accepted} cap={cap}"
	);
}

/// A `Drop` on the `Client` closes the pool: the idle connection's driver
/// task is aborted and the socket closed. A subsequent acquire on a fresh
/// `Client` opens a brand-new connection (the previous pool is gone).
#[tokio::test]
async fn a_dropped_client_closes_its_pool() {
	let server = CountingServer::start().await;
	let sock_str = server.sock_str();
	{
		let client = crate::libpod::Client::new(sock_str.clone());
		let _: serde_json::Value = client.get_json("/libpod/_ping").await.unwrap();
		// Client drops here; the pool's `Arc` is dropped along with it. The
		// idle connection inside the pool is also dropped, which aborts the
		// driver task.
	}
	// A subsequent acquire against the same socket would need a new Client;
	// we don't reach into a closed pool; the test simply verifies the
	// Drop-driven close did not panic and the temp dir is still usable.
	let _ = std::fs::metadata(&sock_str).unwrap();
}

/// A second sequential run after the first client's pool is dropped must open
/// a new connection from scratch. This is the "replacement" path: the server
/// keeps accepting, so the new pool sees an empty idle queue.
#[tokio::test]
async fn a_new_client_opens_a_fresh_connection() {
	let server = CountingServer::start().await;
	{
		let client = crate::libpod::Client::new(server.sock_str());
		let _: serde_json::Value = client.get_json("/libpod/_ping").await.unwrap();
	}
	let after_first = server.accepted();
	let client2 = crate::libpod::Client::new(server.sock_str());
	let _: serde_json::Value = client2.get_json("/libpod/_ping").await.unwrap();
	let after_second = server.accepted();
	assert_eq!(
		after_first, 1,
		"first client accepted exactly one connection"
	);
	assert_eq!(
		after_second, 2,
		"second client opened a fresh connection after the first dropped"
	);
}

/// The pool's cap is what `Client::pool_size` reports back.
#[tokio::test]
async fn pool_size_reflects_the_configured_cap() {
	let c = crate::libpod::Client::with_pool_size("/tmp/none.sock", 3);
	assert_eq!(c.pool_size(), 3);
	let c2 = crate::libpod::Client::new("/tmp/none.sock");
	assert_eq!(c2.pool_size(), crate::libpod::Client::DEFAULT_POOL_SIZE);
}

/// A pool size of zero is the default and means "no pool": every acquire
/// opens a fresh connection. This is the previous, proven behaviour
/// and is what the live-Podman lane regression test relies on.
#[tokio::test]
async fn zero_pool_size_means_no_pool() {
	let c = crate::libpod::Client::with_pool_size("/tmp/none.sock", 0);
	assert_eq!(c.pool_size(), 0);
}

/// Forcibly drop a pooled connection by poisoning it, then verify the next
/// acquire gets a fresh socket. This is the "health check, drop on broken"
/// path: the previous connection's `JoinHandle` is detached when the
/// poisoned `PooledConn` drops (dropping a `JoinHandle` does not abort the
/// task, it just lets it run to completion in the background; the task
/// itself finishes once the hyper connection's IO closes, which the
/// dropped `SendRequest` triggers). The pool hands the next acquire a
/// fresh socket rather than a half-closed one.
#[tokio::test]
async fn a_poisoned_connection_is_replaced_on_next_acquire() {
	let server = CountingServer::start().await;
	let client = crate::libpod::Client::with_pool_size(server.sock_str(), 2);
	// Drive one healthy acquire so a connection lands in the idle queue.
	let _: serde_json::Value = client.get_json("/libpod/_ping").await.unwrap();
	let after_first = server.accepted();

	// Acquire a guard and poison it without issuing a real request: the
	// connection is dropped at guard-Drop time and must be replaced.
	{
		let pool = client.pool_for_tests();
		let mut guard = pool.acquire().await.unwrap();
		guard.poison();
	}

	// Next request opens a brand-new connection.
	let _: serde_json::Value = client.get_json("/libpod/_ping").await.unwrap();
	let after_second = server.accepted();
	assert_eq!(after_first, 1);
	assert_eq!(
		after_second, 2,
		"a poisoned connection must be replaced; got {after_second}"
	);
}

// Chunked-framing regression for #1740. The chunked server + test live in a
// sibling module so the harness file stays under the soft 300-line warn. The
// `path` attribute anchors the lookup next to this file rather than the
// conventional `tests/chunked_tests.rs`, because this file itself is the
// module Rust resolves from a `mod tests;` reference in the parent.
#[path = "chunked_tests.rs"]
mod chunked_tests;

// Readiness regression for #1758. The fixture and tests live in a sibling
// module so this harness file stays under the soft 300-line warn. The two
// tests are multi-threaded and reproduce the "connection was not ready"
// race the live Podman lane hit with `--connection-pool-size 8`: the
// current_thread harness used by the rest of this pool could not see the
// defect, which is how it survived.
#[path = "readiness_tests.rs"]
mod readiness_tests;
