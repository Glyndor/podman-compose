// The pool's connection-reuse semantics ride on `UnixListener`, which is
// only available on Unix. The pool itself is cross-platform (Windows uses
// a named pipe; see `internal/libpod/client/stream.rs`); the integration
// tests in this file that prove keep-alive reuses a single socket are
// Unix-only by necessity. The `#[cfg(unix)]` here skips the tests on
// Windows CI; the pool's other unit tests (which don't bind a socket)
// still run there.
#![cfg(unix)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

/// Fake libpod server that responds with `Transfer-Encoding: chunked`
/// and inserts a short pause between the headers and the body so the
/// client has a window during which the body is in flight. The pause
/// is what makes the readiness race reproducible on a multi-threaded
/// runtime: the user task and the hyper driver task can each be on a
/// different worker, and the user calling `send_request` for the next
/// request can land before the driver has polled its WRITE side
/// (`proto/h1/dispatch.rs:173-175`) and announced readiness
/// (`client/dispatch.rs:182-189`).
struct ReadinessServer {
	sock_path: std::path::PathBuf,
	accepted: Arc<AtomicUsize>,
	_dir: tempfile::TempDir,
	task: tokio::task::JoinHandle<()>,
}

impl ReadinessServer {
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
						// The window during which the next caller can race
						// for the connection. 1ms is enough to widen the
						// gap between the body completion on the driver's
						// READ path and its WRITE-side readiness poll,
						// while keeping the test fast enough that 5000
						// iterations run inside a CI budget.
						tokio::time::sleep(std::time::Duration::from_micros(500)).await;
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

impl Drop for ReadinessServer {
	fn drop(&mut self) {
		self.task.abort();
	}
}

/// The pool hands a pooled connection back without waiting for hyper
/// to publish readiness. On a multi-threaded runtime, the user task
/// that just finished reading the body can release the guard, reacquire
/// it, and call `send_request` before the hyper driver task on its
/// other worker has polled the WRITE side and announced readiness.
/// `can_send` returns false, `send_request` returns
/// `Canceled, "connection was not ready"` (#1758).
///
/// This test pins that bug to a deterministic cap-one sequential reuse
/// against a chunked fixture: every request after the first races for
/// the single tracked connection, the body pause is the window during
/// which the driver is between its READ poll (which publishes body
/// completion) and its WRITE poll (which announces readiness), and the
/// multi-threaded runtime ensures the user task can be on a different
/// worker than the driver.
///
/// The previous test harness for this pool served `Content-Length`
/// responses in one piece, so the body was never in flight when the
/// guard was released; that is the harness that could not see this
/// defect, and the chunked variant is what made it visible. The fix
/// calls `SendRequest::ready()` before handing a connection out
/// (or before sending on it), so `can_send` only runs against a
/// connection whose driver has caught up.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn sequential_reuse_races_for_readiness() {
	let server = ReadinessServer::start().await;
	let client = crate::libpod::Client::with_pool_size(server.sock_str(), 1);

	// Each iteration is a chance to hit the race: the body just
	// arrived, the user is about to call `send_request` for the next
	// request, and the driver on its worker has not yet published
	// readiness. 5000 iterations × 8 worker threads is enough that
	// at least one races on a Linux runner.
	for i in 0..5000 {
		let result: Result<serde_json::Value, _> = client.get_json("/libpod/_ping").await;
		if let Err(e) = result {
			// The diagnostic that pins the defect is
			// `Canceled, "connection was not ready"` (#1758). Hyper's
			// Display drops the `.with("connection was not ready")`
			// context; it lives in the error's Debug / source chain.
			// `stream_end_kind` classifies it as `"canceled"`. Anything
			// else is a different failure this test is not asserting on.
			let kind = e.stream_end_kind();
			assert_eq!(
				kind, "canceled",
				"request {i} failed with an unexpected kind on a healthy socket: {e:?}"
			);
			// Re-pin the cause on the message: `stream_end_kind` is also
			// `"canceled"` for the unrelated `connection closed`
			// variant, so the Debug string is the only thing that says
			// which one fired. The readiness wording is unique to this
			// defect (#1758).
			let dbg = format!("{e:?}");
			assert!(
				dbg.contains("connection was not ready"),
				"request {i} saw a non-readiness Canceled on a healthy socket: {e:?}"
			);
			panic!(
				"request {i} saw `Canceled, \"connection was not ready\"` on a healthy socket: {e}"
			);
		}
	}

	// Reuse: every request rode the single tracked connection.
	// Without the fix, each failed reuse opens a fresh socket and the
	// server accepts up to 5000 connections; with the fix, it accepts
	// one. The bound is what is deterministic: at least one accepted
	// (the connection was opened) and strictly fewer than the number
	// of requests (at least one was reused).
	let accepted = server.accepted();
	assert!(
		(1..5000).contains(&accepted),
		"the pool must reuse one connection under cap=1 sequential traffic; accepted={accepted}"
	);
}

/// Same defect from a different angle: many tasks race for the
/// tracked connections, each task doing sequential requests. The race
/// fires when one task finishes reading the body, releases the guard,
/// and another task acquires the guard and tries to send before the
/// driver has caught up. This is closer to the live Podman failure
/// shape (the four lifecycle cases are not a single-task workload,
/// and the live run used `--connection-pool-size 8`).
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_reuse_races_for_readiness() {
	let server = ReadinessServer::start().await;
	let client = std::sync::Arc::new(crate::libpod::Client::with_pool_size(server.sock_str(), 4));

	let mut handles = Vec::with_capacity(16);
	for task_id in 0..16 {
		let c = client.clone();
		handles.push(tokio::spawn(async move {
			for i in 0..200 {
				let result: Result<serde_json::Value, _> = c.get_json("/libpod/_ping").await;
				if let Err(e) = result {
					let kind = e.stream_end_kind();
					assert_eq!(
						kind, "canceled",
						"task {task_id} request {i} failed with an unexpected kind: {e}"
					);
					panic!(
						"task {task_id} request {i} saw `Canceled, \"connection was not ready\"`: {e}"
					);
				}
			}
		}));
	}
	for h in handles {
		h.await.unwrap();
	}

	// Reuse, not merely "a connection happened". 3200 requests through
	// a cap-of-four pool must open strictly fewer than 3200 sockets; a
	// pool that gives up on the tracked connections every time would.
	let accepted = server.accepted();
	assert!(
		(1..3200).contains(&accepted),
		"the pool must reuse connections under concurrent traffic; accepted={accepted}"
	);
}
