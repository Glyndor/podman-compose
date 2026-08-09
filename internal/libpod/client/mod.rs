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
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};

use super::error::PodmanError;

mod delete;
mod encode;
mod get;
mod hijack;
mod misc;
mod pool;
mod post;
mod put;
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
				if has_connection_close(&resp) {
					guard.poison();
				}
				Ok(resp)
			}
			Ok(Err(e)) => {
				guard.poison();
				Err(PodmanError::Hyper(e))
			}
			Err(e) => {
				guard.poison();
				Err(e)
			}
		}
	}

	/// Send a request whose response body is a long-lived stream and return
	/// the raw response. The connection is opened outside the buffered pool
	/// and held by the [`Client`] until the [`Client`] drops.
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
				self.streaming.lock().unwrap().push(conn);
				Ok(resp)
			}
			Ok(Err(e)) => {
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

	/// For streaming endpoints, return the response on success or parse the
	/// daemon error body on failure.
	async fn stream_or_err(resp: Response<Incoming>) -> Result<Response<Incoming>> {
		if resp.status().is_success() {
			return Ok(resp);
		}
		let (status, body) = Self::read_body(resp, Some(READ_TIMEOUT)).await?;
		Self::check_status(status, &body)?;
		unreachable!("check_status returns Err for a non-success status")
	}
}

/// Lowest libpod API major version podup supports. Podman 5.x reports `5.x.y`;
/// anything below `5.0` lacks SpecGenerator fields podup relies on.
const MIN_LIBPOD_API_MAJOR: u64 = 5;

/// Whether a `Libpod-API-Version` string (e.g. `"5.0.0"`, `"4.9.3"`) meets the
/// [`MIN_LIBPOD_API_MAJOR`].0 floor.
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
