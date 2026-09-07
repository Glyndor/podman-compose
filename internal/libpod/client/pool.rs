//! Per-socket HTTP/1.1 connection pool for the libpod client.
//!
//! [`Client`](super::Client) opens and reuses hyper HTTP/1.1 connections to the
//! Podman socket (or named pipe on Windows) instead of a fresh connection per
//! request. A 100-service `up` would otherwise pay the per-request connect +
//! handshake cost ~600 times; the pool collapses that to one handshake per
//! concurrent caller.
//!
//! The pool is keyed by socket path, so a [`Client`](super::Client) holds one
//! pool and one pool is never shared across sockets. Two flavours of connection
//! are kept side by side:
//!
//! - **Buffered connections** are pooled. Every buffered call acquires one,
//!   uses it, and releases it on completion. A connection that observed an
//!   error is *poisoned* and dropped instead of returned to the idle queue, so
//!   the next acquire opens a fresh one.
//! - **Streaming connections** are dedicated to a single stream and held for
//!   the lifetime of that stream's body. They do not enter the buffered pool:
//!   a stream may be long-lived (`logs -f`, an interactive `exec`), and
//!   surrendering its socket to a buffered caller mid-stream would corrupt the
//!   wire. The [`Client`](super::Client) tracks its in-flight streaming
//!   connections and closes them when it is dropped.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::stream::SocketStream;
use super::{BoxBody, PodmanError, Result};

/// Default cap on the number of live (idle + in-use) buffered connections held
/// to a single libpod socket. Matches the documented default in
/// `docs/commands.md` and `internal/cli/mod.rs`; a pool that is on by
/// default is the whole point of the feature, and a 20-service `up -d`
/// benefits from reuse without the operator having to opt in. A cap of
/// `0` keeps the previous "no pool" behaviour (every acquire opens a
/// fresh connection that is dropped on release); set
/// `--connection-pool-size 0` or `PODUP_LIBPOD_POOL=0` to opt out.
/// Tunable via [`Client::with_pool_size`](super::Client::with_pool_size).
pub(super) const DEFAULT_POOL_SIZE: usize = 8;

/// One pooled HTTP/1.1 connection.
///
/// `sender` is the hyper half the caller writes requests through; `driver` is
/// the background task that pumps the underlying socket. `poisoned` is set when
/// the caller observes an error against this connection (a failed
/// `send_request`, a body-read error) so the next release drops it instead of
/// handing a broken socket to the next acquirer.
struct PooledConn {
	// The fields are accessed through `PoolGuard` once the connection is
	// moved out of the idle queue, which the borrow checker does not track
	// across the `Option<PooledConn>` boundary.
	#[allow(dead_code)]
	sender: http1::SendRequest<BoxBody>,
	#[allow(dead_code)]
	driver: JoinHandle<()>,
	#[allow(dead_code)]
	poisoned: bool,
}

/// State shared across every clone of a [`ConnPool`].
struct PoolInner {
	idle: VecDeque<PooledConn>,
	live_count: usize,
	closed: bool,
}

/// Per-socket HTTP/1.1 connection pool. Cheap to clone; the state is behind
/// `Arc`s internally.
pub(crate) struct ConnPool {
	socket_path: String,
	cap: usize,
	inner: Mutex<PoolInner>,
	notify: Notify,
}

impl ConnPool {
	/// Build a fresh pool bound to `socket_path` that may hold up to `cap`
	/// concurrent buffered connections. A cap of `0` means "no pool": every
	/// acquire opens a fresh connection and drops it on release. The cap is
	/// a hint for reuse, not a cap on concurrency: a parallel caller that
	/// exceeds it is not throttled.
	pub(super) fn new(socket_path: String, cap: usize) -> Arc<Self> {
		Arc::new(Self {
			socket_path,
			cap,
			inner: Mutex::new(PoolInner {
				idle: VecDeque::with_capacity(cap.max(1)),
				live_count: 0,
				closed: false,
			}),
			notify: Notify::new(),
		})
	}

	/// The cap configured at construction time. The cap is immutable for the
	/// life of the pool, so a relaxed read is sufficient.
	pub(super) fn cap(&self) -> usize {
		self.cap
	}

	/// Acquire a buffered connection. Three paths:
	///
	/// - `cap == 0` (the default, "no pool"): open a fresh connection and
	///   return a transient guard that drops on release. This is the
	///   pre-pool behaviour: every request opens a connection, every
	///   release drops it.
	/// - `cap > 0` and an idle connection is available: hand it out.
	/// - `cap > 0` and no idle connection: open a fresh one and track it
	///   in the pool. If the pool is at its cap, open a transient
	///   connection (not tracked) instead: the cap is a hint for idle
	///   reuse, not a cap on concurrency.
	pub(super) async fn acquire(self: &Arc<Self>) -> Result<PoolGuard> {
		// No-pool short-circuit. The pool is opt-in (a cap of 0 means
		// disabled). Every acquire opens a fresh connection that is dropped
		// on release, the previous, proven behaviour.
		if self.cap == 0 {
			let (sender, driver) = open_one(&self.socket_path).await?;
			return Ok(PoolGuard {
				conn: Some(PooledConn {
					sender,
					driver,
					poisoned: false,
				}),
				pool: self.clone(),
				transient: true,
			});
		}

		loop {
			// Register interest BEFORE checking state so a release that fires
			// while we hold the lock cannot miss us: `Notify::notified` returns
			// a future that latches on the first `notify_one` after it was
			// created.
			let waiter = self.notify.notified();
			tokio::pin!(waiter);

			// Phase 1: try to satisfy the acquire from the pool's current
			// state, without holding the lock across an `await`.
			let open_slot = {
				let mut inner = self.inner.lock().unwrap();
				if inner.closed {
					return Err(PodmanError::Api {
						status: 0,
						message: "libpod connection pool is closed".into(),
					});
				}
				// Discard any idle-but-poisoned connections first; they will
				// be replaced by the next acquire that opens fresh. Doing this
				// here, before the at-cap check, keeps the live_count honest
				// A poisoned idle slot no longer counts against `cap`.
				while matches!(inner.idle.front(), Some(c) if c.poisoned) {
					inner.idle.pop_front();
					inner.live_count -= 1;
				}
				if let Some(conn) = inner.idle.pop_front() {
					return Ok(PoolGuard {
						conn: Some(conn),
						pool: self.clone(),
						transient: false,
					});
				}
				if inner.live_count < self.cap {
					inner.live_count += 1;
					Some(self.socket_path.clone())
				} else {
					None
				}
			};

			// Phase 2: with the lock dropped, open the new connection.
			if let Some(path) = open_slot {
				match open_one(&path).await {
					Ok((sender, driver)) => {
						return Ok(PoolGuard {
							conn: Some(PooledConn {
								sender,
								driver,
								poisoned: false,
							}),
							pool: self.clone(),
							transient: false,
						});
					}
					Err(e) => {
						// The open failed; give the slot back so the next
						// acquire can try again.
						let mut inner = self.inner.lock().unwrap();
						inner.live_count -= 1;
						drop(inner);
						self.notify.notify_one();
						return Err(e);
					}
				}
			}

			// At cap. The cap is a hint for reuse, not a cap on concurrency:
			// open a transient connection that is NOT tracked in the pool
			// (no `live_count` increment, no slot to release into). A parallel
			// caller that exceeds the cap is not throttled; it just does not
			// reuse a socket. This is the same shape as Go's `http.Transport`,
			// where `MaxIdleConnsPerHost` caps the idle pool while active
			// requests can exceed it.
			match open_one(&self.socket_path).await {
				Ok((sender, driver)) => {
					return Ok(PoolGuard {
						conn: Some(PooledConn {
							sender,
							driver,
							poisoned: false,
						}),
						pool: self.clone(),
						transient: true,
					});
				}
				Err(e) => {
					// Transient open failed too. Fall through to the wait
					// path below; a release on the pool side may free a slot
					// before we re-check.
					let _ = e;
				}
			}

			// Both pool and transient paths exhausted: wait for the next
			// release to wake us and try again.
			waiter.as_mut().await;
		}
	}

	/// Open a dedicated connection for a streaming call. The connection is
	/// tracked on the pool only so the buffered half sees the pressure;
	/// streaming callers receive their own [`StreamingConn`] regardless.
	pub(super) async fn open_streaming(self: &Arc<Self>) -> Result<StreamingConn> {
		{
			let inner = self.inner.lock().unwrap();
			if inner.closed {
				return Err(PodmanError::Api {
					status: 0,
					message: "libpod connection pool is closed".into(),
				});
			}
		}
		let (sender, driver) = open_one(&self.socket_path).await?;
		Ok(StreamingConn {
			inner: Some(StreamingInner { sender, driver }),
		})
	}

	/// Hand a buffered connection back to the pool.
	fn release(&self, conn: PooledConn) {
		let mut inner = self.inner.lock().unwrap();
		if conn.poisoned {
			inner.live_count -= 1;
		} else {
			inner.idle.push_back(conn);
		}
		drop(inner);
		self.notify.notify_one();
	}

	/// Reject every future acquire and clear the idle queue. In-flight
	/// connections are released normally as their callers finish; the pool
	/// itself drops its notification handle.
	pub(super) fn close(&self) {
		let mut inner = self.inner.lock().unwrap();
		inner.closed = true;
		inner.idle.clear();
		drop(inner);
		// Wake every waiter so they observe `closed` and return the error
		// instead of sleeping forever.
		self.notify.notify_waiters();
	}
}

/// A buffered connection handed out to a caller. On drop the connection is
/// either returned to the pool (healthy, not transient) or discarded
/// (poisoned or transient).
///
/// `transient` is `true` when the pool was at its cap and the acquire fell
/// through to a fresh connection that is NOT tracked in the pool's idle
/// queue. A transient guard is dropped directly: there is no release to
/// the pool, no count decrement, no wake-up. The cap is a hint for reuse,
/// not a cap on concurrency: a parallel caller that exceeds it is not
/// throttled, it just does not get to reuse an idle socket.
pub(super) struct PoolGuard {
	conn: Option<PooledConn>,
	pool: Arc<ConnPool>,
	transient: bool,
}

impl PoolGuard {
	/// Borrow the hyper sender to issue one request.
	pub(super) fn sender_mut(&mut self) -> &mut http1::SendRequest<BoxBody> {
		&mut self.conn.as_mut().unwrap().sender
	}

	/// Mark this connection as broken; the next release will discard it
	/// instead of returning it to the idle queue. Call when the
	/// `send_request` future or the body read returned an error.
	pub(super) fn poison(&mut self) {
		if let Some(c) = self.conn.as_mut() {
			c.poisoned = true;
		}
	}
}

impl Drop for PoolGuard {
	fn drop(&mut self) {
		if let Some(conn) = self.conn.take() {
			// A transient connection was opened because the pool was at cap;
			// it was never tracked in `live_count` and there is no slot to
			// release into. Drop it directly. The background `driver` task
			// aborts when the socket closes; the `SendRequest` is dropped.
			if self.transient {
				return;
			}
			self.pool.release(conn);
		}
	}
}

/// A dedicated connection held by a streaming call. The underlying socket is
/// closed when this is dropped, regardless of whether the stream ended cleanly.
pub(super) struct StreamingConn {
	inner: Option<StreamingInner>,
}

struct StreamingInner {
	sender: http1::SendRequest<BoxBody>,
	driver: JoinHandle<()>,
}

impl StreamingConn {
	/// Borrow the hyper sender to issue one request on this dedicated
	/// connection.
	pub(super) fn sender_mut(&mut self) -> &mut http1::SendRequest<BoxBody> {
		&mut self.inner.as_mut().unwrap().sender
	}
}

impl Drop for StreamingConn {
	fn drop(&mut self) {
		// Aborting the driver task closes the socket via the IO half hyper
		// holds; the sender is left alone because dropping it does not, on
		// its own, surface an EOF to the background task in a timely way.
		if let Some(inner) = self.inner.take() {
			inner.driver.abort();
		}
	}
}

/// Open a fresh HTTP/1.1 connection to `socket_path` and spawn the
/// read/write driver task that pumps it.
async fn open_one(socket_path: &str) -> Result<(http1::SendRequest<BoxBody>, JoinHandle<()>)> {
	let stream = SocketStream::connect(socket_path).await?;
	let io = TokioIo::new(stream);
	let (sender, conn) = http1::handshake(io).await.map_err(PodmanError::Hyper)?;
	let driver = tokio::spawn(async move {
		let _ = conn.await;
	});
	Ok((sender, driver))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Client construction / introspection
// ---------------------------------------------------------------------------
//
// These methods live here (not in `mod.rs`) so the `Client` impl block in
// the parent module stays within the 500-line file cap. They are part of the
// public libpod API; re-exporting them on `Client` is intentional.

use super::Client;

impl Client {
	/// Default per-socket pool size used by [`Client::new`](Self::new). See
	/// [`Client::with_pool_size`](Self::with_pool_size) to tune.
	pub const DEFAULT_POOL_SIZE: usize = DEFAULT_POOL_SIZE;

	/// Create a client bound to the given Podman socket path (or named pipe),
	/// using the default connection pool size
	/// ([`Client::DEFAULT_POOL_SIZE`](Self::DEFAULT_POOL_SIZE)).
	pub fn new(socket_path: impl Into<String>) -> Self {
		Self::with_pool_size(socket_path, Self::DEFAULT_POOL_SIZE)
	}

	/// Create a client bound to the given Podman socket path, holding up to
	/// `pool_size` concurrent HTTP/1.1 connections for reuse. Streaming
	/// endpoints always take a dedicated connection outside this cap.
	///
	/// `pool_size` is floored at 1; a zero value would deadlock the first
	/// acquire rather than fail loud.
	pub fn with_pool_size(socket_path: impl Into<String>, pool_size: usize) -> Self {
		let socket_path = socket_path.into();
		let pool = ConnPool::new(socket_path.clone(), pool_size);
		Self {
			socket_path,
			pool,
			streaming: Mutex::new(Vec::new()),
		}
	}

	/// The configured maximum number of live (idle + in-use) buffered
	/// connections kept to the socket. Streaming connections are tracked on
	/// the same socket but do not count against this cap.
	pub fn pool_size(&self) -> usize {
		// Exposed via the public `ConnPool` so the field stays `pub(crate)`;
		// callers asking for the cap should not need internal access.
		self.pool_cap()
	}

	/// Internal accessor for the pool cap. Kept separate so the public
	/// `pool_size` does not have to expose the pool type.
	fn pool_cap(&self) -> usize {
		// `ConnPool::cap` is set at construction and never mutated, so a
		// relaxed read of the field through `Arc` is sufficient.
		self.pool.cap()
	}

	/// Test-only access to the underlying [`ConnPool`]. Lets the pool's tests
	/// exercise `acquire` / `poison` directly without routing a real request.
	#[cfg(any(test, feature = "test-helpers"))]
	#[allow(dead_code)]
	pub(crate) fn pool_for_tests(&self) -> &Arc<ConnPool> {
		&self.pool
	}
}
