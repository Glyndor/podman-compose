//! HTTP client for the Podman libpod REST API.
//!
//! Reuses HTTP/1.1 connections to the Podman Unix socket (or named pipe on
//! Windows) across requests through the per-socket pool in
//! [`client::pool`](self). Buffered calls acquire a connection, issue one
//! request, and release it on completion; streaming calls take a dedicated
//! connection for the lifetime of the stream and release it when the body
//! drops. See [`Client`] for the full contract.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::Stream;
use http_body_util::{BodyExt, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::{Method, Request, Response, StatusCode};
use serde::{de::DeserializeOwned, Serialize};

use super::error::PodmanError;

mod encode;
mod hijack;
mod pool;
mod stream;
pub(crate) use encode::{is_valid_object_name, urlencoded};
pub(crate) use hijack::Hijacked;
use pool::ConnPool;
use stream::SocketStream;

/// The request body every call shares. A boxed body so a fully-buffered
/// `Full<Bytes>` (almost every call) and a lazily-streamed build-context body
/// (the `build` endpoint) travel the same client path. `Unsync` because hyper's
/// `send_request` only requires the body to be `Send`, and the streamed body is
/// not `Sync`.
type BoxBody = http_body_util::combinators::UnsyncBoxBody<Bytes, std::io::Error>;

/// Box a fully-buffered byte payload into [`BoxBody`]. `Full`'s error is
/// `Infallible`, mapped to the unified `io::Error` (which it never produces).
fn full(bytes: Bytes) -> BoxBody {
	Full::new(bytes)
		.map_err(|never| match never {})
		.boxed_unsync()
}

/// Upper bound on a buffered (non-streaming) response body. Caps memory use
/// when the daemon returns an oversized or runaway response.
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Ceiling on establishing the socket connection and HTTP handshake. Bounds the
/// wait when the Podman socket is absent, busy, or unresponsive. This times the
/// connect only — it does not limit the duration of a streaming response body.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Ceiling on reading a *buffered* (non-streaming) response body. Without it a
/// daemon that accepts the request, sends headers, then stalls would hang the
/// CLI forever. Streaming helpers (logs, attach, archive) are deliberately not
/// bounded by this — they are long-lived by design.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Whether a response carried `Connection: close`. When set, the socket is
/// unusable for any further request and the pool must discard it instead of
/// handing it back to the next acquirer. HTTP/1.1 keep-alive is the default
/// in podup's real wire path; a `close` value is the server telling us this
/// socket is single-use.
fn has_connection_close(resp: &Response<Incoming>) -> bool {
	resp.headers()
		.get(hyper::header::CONNECTION)
		.and_then(|v| v.to_str().ok())
		.map(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("close")))
		.unwrap_or(false)
}

/// Result alias for libpod client calls, fixing the error to [`PodmanError`].
pub type Result<T> = std::result::Result<T, PodmanError>;

/// Podman libpod REST API client.
///
/// Holds an HTTP/1.1 connection pool keyed by socket path. Buffered calls
/// acquire a connection, issue one request, and release the connection on
/// completion; connections that observed an error are dropped instead of
/// returned to the pool. Streaming calls (`get_stream`, `post_json_stream`,
/// `post_empty_stream`, `post_bytes_stream`, `post_stream_body`,
/// `post_json_stream_within`) take a dedicated connection for the lifetime of
/// the stream's response body. Streaming connections do not share with the
/// buffered pool — they are released when the [`Client`] is dropped, which in
/// the CLI is the end of the command.
pub struct Client {
	socket_path: String,
	pool: Arc<ConnPool>,
	streaming: Mutex<Vec<pool::StreamingConn>>,
}

/// The decoded `X-Docker-Container-Path-Stat` header — a container path's name,
/// size, Go file `mode` and `mtime`.
///
/// `mtime` is an RFC3339 string compared only for equality, never parsed into a
/// time. **Podman 6 reports it to whole seconds** — `2026-08-03T18:36:05Z`, no
/// fractional part, measured on `podman-6.0.1-1.fc45` — which is why `size` is
/// carried here too: two writes inside one second are indistinguishable by mtime
/// alone. The runtime's JSON uses lowercase keys.
#[derive(serde::Deserialize, Default, Clone, PartialEq, Eq, Debug)]
pub(crate) struct PathStat {
	#[serde(default)]
	pub(crate) size: u64,
	#[serde(default)]
	pub(crate) mode: u64,
	#[serde(default)]
	pub(crate) mtime: String,
}

/// Attach the socket path and a way forward to a connection failure.
///
/// The operator saw `podman socket connection error: No such file or directory
/// (os error 2)` — no path, no distinction between "it is not there" and "I
/// cannot open it", and nothing to do about it. Everything needed was already
/// in hand one call earlier (#1146).
///
/// The path is folded into the `io::Error`'s message rather than into a new
/// error variant so `PodmanError` keeps its shape: it is public API, frozen
/// since 2.0.0. `kind()` survives, which is what tells the two cases apart.
///
/// Unix only because the hints are: `systemctl --user` means nothing to a
/// `podman machine` install, and the named-pipe connect path reports its
/// errors raw.
#[cfg(unix)]
pub(crate) fn socket_error(path: &str, e: std::io::Error) -> super::PodmanError {
	let hint = match e.kind() {
		std::io::ErrorKind::NotFound => {
			" — the Podman API socket is not listening. podman itself is daemonless \
			 and needs no socket, but podup speaks the libpod API and does. Enable it \
			 with `systemctl --user enable --now podman.socket`, or for an account \
			 with no login shell: `sudo -u <user> env XDG_RUNTIME_DIR=/run/user/$(id \
			 -u <user>) systemctl --user enable --now podman.socket`"
		}
		std::io::ErrorKind::PermissionDenied => {
			" — the socket exists but cannot be opened. Check that it is owned by \
			 the user running podup; a socket created by another account is not \
			 shared"
		}
		_ => "",
	};
	super::PodmanError::Connect(std::io::Error::new(e.kind(), format!("{path}: {e}{hint}")))
}

impl Drop for Client {
	/// Close every held connection. Idle pooled connections are dropped via
	/// the pool's `close`, which wakes any blocked acquirers with a closed
	/// error; streaming connections are dropped directly, aborting their
	/// driver tasks and tearing down their sockets.
	fn drop(&mut self) {
		// Clear the streaming connections first so the drop of each
		// `StreamingConn` runs while the pool is still around. The pool's
		// `close` then drains the idle queue.
		self.streaming.lock().unwrap().clear();
		self.pool.close();
	}
}

impl Client {
	/// Build a request with an optional JSON body.
	fn build_request(
		method: Method,
		path: &str,
		body: BoxBody,
		content_type: Option<&str>,
	) -> Result<Request<BoxBody>> {
		let uri: hyper::Uri = format!("http://localhost{path}").parse().map_err(
			|e: hyper::http::uri::InvalidUri| PodmanError::Api {
				status: 0,
				message: format!("invalid API path '{path}': {e}"),
			},
		)?;

		let mut builder = Request::builder()
			.method(method)
			.uri(uri)
			.header(hyper::header::HOST, "localhost");

		if let Some(ct) = content_type {
			builder = builder.header(hyper::header::CONTENT_TYPE, ct);
		}

		builder.body(body).map_err(|e| PodmanError::Api {
			status: 0,
			message: e.to_string(),
		})
	}

	/// Send a request and return the raw response.
	///
	/// `response_timeout` bounds how long we wait for the server to return the
	/// response head. Pass `Some` (the default [`READ_TIMEOUT`]) for ordinary and
	/// streaming calls, where the head arrives promptly — this stops a socket that
	/// accepts the connection but never replies from hanging the CLI indefinitely.
	/// Pass `None` only for endpoints that legitimately block server-side before
	/// the head (e.g. `wait?condition=stopped`), whose callers impose an outer
	/// budget.
	async fn send(
		&self,
		req: Request<BoxBody>,
		response_timeout: Option<std::time::Duration>,
	) -> Result<Response<Incoming>> {
		tracing::debug!("libpod {} {}", req.method(), req.uri().path());
		// Acquire a pooled connection, bounded by the connect-timeout so a
		// stuck or absent socket cannot park the call indefinitely.
		let mut guard = tokio::time::timeout(CONNECT_TIMEOUT, self.pool.acquire())
			.await
			.map_err(|_| PodmanError::Api {
				status: 0,
				message: format!(
					"timed out after {}s connecting to the Podman socket",
					CONNECT_TIMEOUT.as_secs()
				),
			})??;
		let request = guard.sender_mut().send_request(req);
		let send_result = Self::apply_timeout(
			response_timeout,
			"waiting for the Podman socket to respond",
			request,
		)
		.await;
		match send_result {
			Ok(Ok(resp)) => {
				// The peer signalled it is closing the socket — drop the
				// connection proactively instead of letting the next
				// acquire discover it half-closed and pay the wait.
				if has_connection_close(&resp) {
					guard.poison();
				}
				Ok(resp)
			}
			Ok(Err(e)) => {
				// The head never came back. The connection is no longer
				// usable; poison so the next release drops it instead of
				// handing a dead socket to the next acquirer.
				guard.poison();
				Err(PodmanError::Hyper(e))
			}
			Err(e) => {
				// The timeout fired before the head arrived. Same outcome:
				// the connection cannot be safely reused, drop it.
				guard.poison();
				Err(e)
			}
		}
	}

	/// Send a request whose response body is a long-lived stream and return
	/// the raw response. The connection is opened outside the buffered pool
	/// and held by the [`Client`] until the [`Client`] drops — the streaming
	/// body returned to the caller reads from this held connection, and
	/// surrendering it to a buffered caller mid-stream would corrupt the
	/// wire. Closing the [`Client`] (e.g. at the end of a CLI command) tears
	/// the connection down.
	async fn send_streaming(
		&self,
		req: Request<BoxBody>,
		response_timeout: Option<std::time::Duration>,
	) -> Result<Response<Incoming>> {
		tracing::debug!("libpod {} {}", req.method(), req.uri().path());
		let mut conn = tokio::time::timeout(CONNECT_TIMEOUT, self.pool.open_streaming())
			.await
			.map_err(|_| PodmanError::Api {
				status: 0,
				message: format!(
					"timed out after {}s connecting to the Podman socket",
					CONNECT_TIMEOUT.as_secs()
				),
			})??;
		let request = conn.sender_mut().send_request(req);
		let send_result = Self::apply_timeout(
			response_timeout,
			"waiting for the Podman socket to respond",
			request,
		)
		.await;
		match send_result {
			Ok(Ok(resp)) => {
				// Hand the connection to the [`Client`] for the lifetime of
				// the response body; the body's [`Incoming`] reads from this
				// socket. Dropping the body itself does not close the
				// connection — only the [`Client`] Drop does — because the
				// streaming helpers' return type is fixed at `Response<Incoming>`
				// and we cannot attach a hook to the body drop without
				// changing the public surface.
				self.streaming.lock().unwrap().push(conn);
				Ok(resp)
			}
			Ok(Err(e)) => {
				// Head never came; the [`StreamingConn`] drop will close the
				// socket. No further tracking needed.
				drop(conn);
				Err(PodmanError::Hyper(e))
			}
			Err(e) => {
				drop(conn);
				Err(e)
			}
		}
	}

	/// Read the full response body into a `Vec<u8>`, capped at
	/// [`MAX_RESPONSE_BYTES`] so a rogue or runaway daemon cannot exhaust memory.
	///
	/// `read_timeout` bounds how long we wait for the body. Pass `Some` to apply a
	/// ceiling (the default [`READ_TIMEOUT`] for ordinary buffered calls); pass
	/// `None` for endpoints that legitimately block server-side for an unbounded
	/// duration (e.g. `wait?condition=stopped`, where the caller imposes its own
	/// outer budget).
	async fn read_body(
		resp: Response<Incoming>,
		read_timeout: Option<std::time::Duration>,
	) -> Result<(StatusCode, Vec<u8>)> {
		let status = resp.status();
		let read = Limited::new(resp.into_body(), MAX_RESPONSE_BYTES).collect();
		let collected = Self::apply_timeout(
			read_timeout,
			"reading the response body from the Podman socket",
			read,
		)
		.await?
		.map_err(|e| PodmanError::Api {
			status: 0,
			message: format!("reading response body: {e}"),
		})?;
		Ok((status, collected.to_bytes().to_vec()))
	}

	/// Await `fut`, optionally bounded by `timeout`.
	///
	/// With `Some(limit)` a stalled future is aborted once `limit` elapses, yielding
	/// a timeout [`PodmanError`] whose message names `phase` (what we were waiting
	/// on); with `None` it is awaited uncapped, for endpoints that legitimately
	/// block server-side (the caller supplies its own outer budget). Shared by the
	/// response-head wait ([`send`](Self::send)) and the body read
	/// ([`read_body`](Self::read_body)); split out so the policy is testable without
	/// a live socket.
	async fn apply_timeout<F, T>(
		timeout: Option<std::time::Duration>,
		phase: &str,
		fut: F,
	) -> Result<T>
	where
		F: std::future::Future<Output = T>,
	{
		match timeout {
			Some(limit) => tokio::time::timeout(limit, fut)
				.await
				.map_err(|_| PodmanError::Api {
					status: 0,
					message: format!("timed out after {}s {phase}", limit.as_secs()),
				}),
			None => Ok(fut.await),
		}
	}

	/// Check status code; on error parse the Podman error message.
	fn check_status(status: StatusCode, body: &[u8]) -> Result<()> {
		if status.is_success() {
			return Ok(());
		}

		#[derive(serde::Deserialize)]
		struct ApiError {
			cause: Option<String>,
			message: Option<String>,
		}

		let msg = if let Ok(e) = serde_json::from_slice::<ApiError>(body) {
			e.message
				.or(e.cause)
				.unwrap_or_else(|| String::from_utf8_lossy(body).into_owned())
		} else {
			String::from_utf8_lossy(body).into_owned()
		};

		Err(PodmanError::Api {
			status: status.as_u16(),
			message: msg,
		})
	}

	/// For streaming endpoints: return the response on success, otherwise read
	/// the body and surface it through [`check_status`](Self::check_status) so the caller gets the
	/// parsed Podman error message rather than the raw JSON body.
	async fn stream_or_err(resp: Response<Incoming>) -> Result<Response<Incoming>> {
		if resp.status().is_success() {
			return Ok(resp);
		}
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		unreachable!("check_status returns Err for a non-success status")
	}

	// ---------------------------------------------------------------------------
	// Request helpers
	// ---------------------------------------------------------------------------

	/// `GET /libpod/_ping` — returns Ok(()) when Podman is reachable *and* speaks
	/// a libpod API version podup supports.
	///
	/// Podman answers `_ping` with a `Libpod-API-Version` response header. We read
	/// it here, while the call is already cheap, and reject a server below the
	/// `MIN_LIBPOD_API_MAJOR.0` floor with a clear
	/// `PodmanError::IncompatibleApiVersion` rather than letting a later
	/// SpecGenerator or libpod-native call fail with an obscure 4xx.
	pub async fn ping(&self) -> Result<()> {
		// Deliberately omits the version prefix: `_ping` is version-independent.
		let req = Self::build_request(Method::GET, "/libpod/_ping", full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		// Read the version header before the body is consumed below.
		let reported = resp
			.headers()
			.get("Libpod-API-Version")
			.and_then(|v| v.to_str().ok())
			.unwrap_or_default()
			.to_owned();
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		if !meets_minimum(&reported) {
			return Err(PodmanError::IncompatibleApiVersion { reported });
		}
		Ok(())
	}

	/// `GET` → deserialize JSON response.
	pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
		let req = Self::build_request(Method::GET, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(PodmanError::Json)
	}

	/// `GET` → return raw `Response<Incoming>` for streaming.
	pub async fn get_stream(&self, path: &str) -> Result<Response<Incoming>> {
		let req = Self::build_request(Method::GET, path, full(Bytes::new()), None)?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with JSON body → deserialize JSON response.
	pub async fn post_json<B: Serialize, T: DeserializeOwned>(
		&self,
		path: &str,
		body: &B,
	) -> Result<T> {
		let json = serde_json::to_vec(body).map_err(PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(PodmanError::Json)
	}

	/// `POST` with JSON body → ignore response body (expect 2xx).
	pub async fn post_json_ok<B: Serialize>(&self, path: &str, body: &B) -> Result<()> {
		let json = serde_json::to_vec(body).map_err(PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)
	}

	/// `POST` with JSON body → return raw `Response<Incoming>` for streaming.
	pub async fn post_json_stream<B: Serialize>(
		&self,
		path: &str,
		body: &B,
	) -> Result<Response<Incoming>> {
		let json = serde_json::to_vec(body).map_err(PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with empty body → ignore response body (expect 2xx or 304).
	pub async fn post_empty_ok(&self, path: &str) -> Result<()> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		// 304 Not Modified is fine for idempotent ops
		if status == StatusCode::NOT_MODIFIED {
			return Ok(());
		}
		Self::check_status(status, &body)
	}

	/// `POST` with empty body → ignore response body (expect 2xx or 304), bounded
	/// by a caller-chosen deadline rather than the default `READ_TIMEOUT`.
	///
	/// `deadline` of `Some` caps both the response-head wait and the body read so a
	/// `stop` on a container that is slow to die (or a wedged libpod call) returns a
	/// timeout error after the grace window instead of pinning the CLI for the full
	/// `READ_TIMEOUT`; `None` leaves it uncapped (docker `stop -t -1` parity). The
	/// caller decides whether a resulting `PodmanError::is_timeout` warrants a
	/// client-side `SIGKILL`/force-remove escalation.
	pub async fn post_empty_ok_within(
		&self,
		path: &str,
		deadline: Option<std::time::Duration>,
	) -> Result<()> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, deadline).await?;
		let (status, body) = Self::read_body(resp, deadline).await?;
		// 304 Not Modified is fine for idempotent ops
		if status == StatusCode::NOT_MODIFIED {
			return Ok(());
		}
		Self::check_status(status, &body)
	}

	/// `POST` with JSON body → return raw `Response<Incoming>` for streaming,
	/// bounding the wait for the response head by `head_timeout` instead of the
	/// default `READ_TIMEOUT`.
	///
	/// `exec`-start uses this with a short, exec-specific ceiling: a healthy engine
	/// returns the start head (the hijack, or a prompt error) almost immediately, so
	/// a long wait means the launch is wedged — e.g. a nonexistent target user the
	/// server stalls resolving. Bounding the head lets the caller fail fast with a
	/// clear, exec-specific message rather than pinning the CLI for the full
	/// `READ_TIMEOUT` and then reporting a misleading socket-timeout. The streamed
	/// body is left unbounded (`head_timeout` covers only the head), so a legitimate
	/// long-running exec still streams normally.
	pub async fn post_json_stream_within<B: Serialize>(
		&self,
		path: &str,
		body: &B,
		head_timeout: Option<std::time::Duration>,
	) -> Result<Response<Incoming>> {
		let json = serde_json::to_vec(body).map_err(PodmanError::Json)?;
		let req = Self::build_request(
			Method::POST,
			path,
			full(Bytes::from(json)),
			Some("application/json"),
		)?;
		Self::stream_or_err(self.send_streaming(req, head_timeout).await?).await
	}

	/// `POST` with empty body → return raw `Response<Incoming>` for streaming.
	pub async fn post_empty_stream(&self, path: &str) -> Result<Response<Incoming>> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with empty body → deserialize JSON response.
	pub async fn post_empty_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(PodmanError::Json)
	}

	/// `POST` with empty body → deserialize JSON response, with **no** read-timeout
	/// ceiling on the response body.
	///
	/// For blocking endpoints that legitimately hold the connection open for an
	/// arbitrary, server-side duration — notably `containers/{name}/wait`, which
	/// does not respond until the container reaches the requested condition. The
	/// default `READ_TIMEOUT` would otherwise abort the call after 120 s and
	/// surface a spurious timeout instead of the real exit code, so callers of
	/// this method must impose their own outer budget (e.g. a
	/// [`tokio::time::timeout`]) to stay bounded.
	pub async fn post_empty_json_unbounded<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
		let req = Self::build_request(Method::POST, path, full(Bytes::new()), None)?;
		let resp = self.send(req, None).await?;
		let (status, body) = Self::read_body(resp, None).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(PodmanError::Json)
	}

	/// `POST` with raw bytes body → return raw `Response<Incoming>` for streaming.
	pub async fn post_bytes_stream(
		&self,
		path: &str,
		bytes: Bytes,
		content_type: &str,
	) -> Result<Response<Incoming>> {
		let req = Self::build_request(Method::POST, path, full(bytes), Some(content_type))?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with a **streamed** body → return raw `Response<Incoming>` for
	/// streaming.
	///
	/// The body is produced lazily from `chunks` rather than buffered whole, so a
	/// large upload (a multi-gigabyte build-context tar) never inflates the
	/// process's RSS. Each item is an `http_body`-style frame or a terminal
	/// `io::Error` that aborts the request.
	pub async fn post_stream_body<S>(
		&self,
		path: &str,
		chunks: S,
		content_type: &str,
	) -> Result<Response<Incoming>>
	where
		S: Stream<Item = std::result::Result<Frame<Bytes>, std::io::Error>> + Send + 'static,
	{
		let body = StreamBody::new(chunks).boxed_unsync();
		let req = Self::build_request(Method::POST, path, body, Some(content_type))?;
		Self::stream_or_err(self.send_streaming(req, Some(READ_TIMEOUT)).await?).await
	}

	/// `POST` with a raw-bytes body → deserialize JSON response.
	///
	/// Used by endpoints that take a binary payload rather than a JSON object —
	/// e.g. `secrets/create`, whose body is the raw secret data and whose
	/// response is `{"ID": "..."}`.
	pub async fn post_bytes_json<T: DeserializeOwned>(
		&self,
		path: &str,
		bytes: Bytes,
		content_type: &str,
	) -> Result<T> {
		let req = Self::build_request(Method::POST, path, full(bytes), Some(content_type))?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		serde_json::from_slice(&body).map_err(PodmanError::Json)
	}

	/// `PUT` with raw bytes body → expect 2xx.
	///
	/// #1097 lives on top of this call: the container-archive PUT on Podman 6
	/// applies the tar and then closes the connection before completing the
	/// response, which surfaces here as an `IncompleteMessage`. The recovery — the
	/// endpoint applies the upload, so re-verify the destination changed rather
	/// than fail a copy that landed — belongs to the caller, which knows the
	/// destination (`Engine::put_archive_verified`). This method just reports the
	/// outcome; the caller keys the recovery off
	/// `PodmanError::is_incomplete_message`.
	pub async fn put_bytes_ok(&self, path: &str, bytes: Bytes, content_type: &str) -> Result<()> {
		let len = bytes.len();
		let req = Self::build_request(Method::PUT, path, full(bytes), Some(content_type))?;
		let resp = match self.send(req, Some(READ_TIMEOUT)).await {
			Ok(r) => r,
			Err(e) => {
				// Debug, not warn: `cp` handles the Podman-6 IncompleteMessage on
				// this endpoint by re-verifying the copy landed (#1097), so a
				// warning here would cry "failed" on a copy that succeeded. A
				// genuinely failed PUT surfaces through the returned error.
				tracing::debug!(
					"PUT {path} ({content_type}, {len} bytes) ended [{}]: {e}",
					e.stream_end_kind()
				);
				return Err(e);
			}
		};
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)
	}

	/// `HEAD` a container-archive path and decode its `X-Docker-Container-Path-Stat`
	/// header, returning `Some(stat)` when the path exists or `None` on 404. The
	/// header is base64 JSON carrying the Go file `mode`, `size` and `mtime`.
	/// Shared by [`head_path_is_dir`](Self::head_path_is_dir) and
	/// [`head_path_stat`](Self::head_path_stat).
	async fn head_container_path_stat(&self, path: &str) -> Result<Option<PathStat>> {
		use base64::Engine as _;

		let req = Self::build_request(Method::HEAD, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let status = resp.status();
		if status == StatusCode::NOT_FOUND {
			return Ok(None);
		}
		let stat = resp
			.headers()
			.get("X-Docker-Container-Path-Stat")
			.and_then(|v| v.to_str().ok())
			.map(str::to_string);
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		if status == StatusCode::NOT_FOUND {
			return Ok(None);
		}
		Self::check_status(status, &body)?;
		// The path exists but the runtime sent no stat header: report existence
		// with a zeroed stat rather than failing (matches the prior behaviour,
		// which treated a missing header as "exists, not a directory").
		let Some(stat) = stat else {
			return Ok(Some(PathStat::default()));
		};
		let json = base64::engine::general_purpose::STANDARD
			.decode(stat.as_bytes())
			.map_err(|e| PodmanError::Api {
				status: 0,
				message: format!("malformed container path stat: {e}"),
			})?;
		Ok(Some(
			serde_json::from_slice(&json).map_err(PodmanError::Json)?,
		))
	}

	/// `HEAD` a container-archive path, returning `Some(is_dir)` when it exists or
	/// `None` on 404. Lets `cp` tell an existing destination directory (copy into
	/// it) from a target name (rename on copy), matching `docker cp`.
	pub async fn head_path_is_dir(&self, path: &str) -> Result<Option<bool>> {
		// Go's os.ModeDir is the high bit of the 32-bit FileMode.
		Ok(self
			.head_container_path_stat(path)
			.await?
			.map(|s| s.mode & (1 << 31) != 0))
	}

	/// The full decoded stat for a container path, or `None` when it does not
	/// exist.
	///
	/// `cp` needs the size as well as the mtime: Podman 6's mtime has
	/// one-second resolution, so a second copy inside the same second cannot be
	/// told from a failed one by mtime alone.
	pub(crate) async fn head_path_stat(&self, path: &str) -> Result<Option<PathStat>> {
		self.head_container_path_stat(path).await
	}

	/// `DELETE` → `Ok(true)` if the resource existed and was removed, `Ok(false)`
	/// on a 404 (nothing to delete). Lets a caller tell a real deletion from a
	/// no-op, so it can avoid reporting a phantom "removed" for a container that
	/// never existed.
	pub async fn delete_existed(&self, path: &str) -> Result<bool> {
		let req = Self::build_request(Method::DELETE, path, full(Bytes::new()), None)?;
		let resp = self.send(req, Some(READ_TIMEOUT)).await?;
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		if status == StatusCode::NOT_FOUND {
			return Ok(false);
		}
		Self::check_status(status, &body)?;
		Ok(true)
	}

	/// `DELETE` → ignore response body (expect 2xx or 404). A 404 is an
	/// idempotent no-op; see [`Self::delete_existed`] when the distinction matters.
	pub async fn delete_ok(&self, path: &str) -> Result<()> {
		self.delete_existed(path).await.map(|_| ())
	}
}

/// Lowest libpod API major version podup supports. Podman 5.x reports `5.x.y`;
/// anything below `5.0` lacks SpecGenerator fields podup relies on.
const MIN_LIBPOD_API_MAJOR: u64 = 5;

/// Whether a `Libpod-API-Version` string (e.g. `"5.0.0"`, `"4.9.3"`) meets the
/// [`MIN_LIBPOD_API_MAJOR`].0 floor.
///
/// Pure and total so it is unit-testable in isolation. Only the major component
/// gates: any `5.x.y` (or higher major) passes; `4.x.y` is rejected. An empty or
/// malformed string — a server that sent no header, or a value we cannot parse —
/// is treated as *not* meeting the minimum, so we fail closed rather than assume
/// a compatible server.
fn meets_minimum(version: &str) -> bool {
	version
		.trim()
		.trim_start_matches('v')
		.split('.')
		.next()
		.and_then(|major| major.parse::<u64>().ok())
		.is_some_and(|major| major >= MIN_LIBPOD_API_MAJOR)
}

#[cfg(test)]
mod tests;
