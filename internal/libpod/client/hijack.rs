//! Raw bidirectional streams over the libpod socket, for interactive exec.
//!
//! The rest of the client is connection-per-request through hyper, which is the
//! right shape for a CLI: send, read, done. An interactive exec is not that
//! shape. `POST /exec/{id}/start` with a TTY keeps the connection open in both
//! directions for as long as the command runs (the caller's keystrokes go up
//! while the command's output comes down) so there is no response to read and
//! return.
//!
//! Rather than teach the hyper path to hijack a connection, this writes the
//! request by hand and hands back the socket. The request is trivial (one path,
//! one short JSON body, no redirects, no keep-alive) and hand-writing it keeps
//! the upgrade out of the general client, which every other call would then have
//! to reason about.

use tokio::io::{AsyncRead, AsyncWriteExt};

use super::{Client, PodmanError, Result, SocketStream};

/// Cap on the response head podup will read before deciding the server is not
/// speaking HTTP. A hijacked stream's head is a status line and a handful of
/// headers; anything larger is a malformed or hostile peer, and reading it
/// unbounded would be a trivial memory exhaustion.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// Connect budget for the raw hijack. Mirrors the hyper path's
/// [`super::CONNECT_TIMEOUT`] so a stuck or absent peer surfaces the same
/// 30-second ceiling here as on every other libpod call.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Read budget for the response head. Mirrors the hyper path's
/// [`super::READ_TIMEOUT`]. The full 120s is excessive for a status line;
/// a 30s ceiling matches what `connect` does on the same path and bounds a
/// peer that opens the TCP/HTTP/1.1 handshake but never answers.
const HEAD_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A hijacked connection: the socket, after the response head has been read.
///
/// Bytes written go to the command's stdin; bytes read are its output. With a
/// TTY the stream is raw (no 8-byte frame headers) because the pty merges
/// stdout and stderr, which is also why an interactive exec cannot separate
/// them; the same is true of `podman exec -it`.
#[derive(Debug)]
pub(crate) struct Hijacked {
	pub(crate) stream: SocketStream,
}

impl Client {
	/// `POST` a JSON body and keep the connection, for a stream that talks back.
	///
	/// Returns once the response head is read, so a rejected exec (404, 409)
	/// surfaces as an error instead of hanging with the terminal already in raw
	/// mode, which would leave the user's shell unusable.
	pub(crate) async fn post_hijack(&self, path: &str, body: &[u8]) -> Result<Hijacked> {
		// A stuck or absent peer would otherwise wedge `connect` until the
		// OS gives up on it. The hyper callers already wrap their pool
		// acquisition in `tokio::time::timeout(CONNECT_TIMEOUT, ...)`; the
		// hijack path bypasses the pool, so the same ceiling has to land
		// here. Windows already retries `ERROR_PIPE_BUSY` inside
		// `SocketStream::connect`; `tokio::time::timeout` adds the wall-clock
		// ceiling that retry loop does not.
		let stream =
			match tokio::time::timeout(CONNECT_TIMEOUT, SocketStream::connect(&self.socket_path))
				.await
			{
				Ok(Ok(s)) => s,
				Ok(Err(e)) => return Err(e),
				Err(_) => {
					return Err(PodmanError::Api {
						status: 0,
						message: format!(
							"exec start connect timed out after {} seconds",
							CONNECT_TIMEOUT.as_secs()
						),
					});
				}
			};
		let mut stream = stream;

		// `Connection: close` is deliberate: this socket is never returned to a
		// pool, and saying so stops the server holding it open after the command
		// exits.
		let head = format!(
			"POST {path} HTTP/1.1\r\n\
			 Host: localhost\r\n\
			 Content-Type: application/json\r\n\
			 Content-Length: {}\r\n\
			 Connection: close\r\n\
			 \r\n",
			body.len()
		);
		stream.write_all(head.as_bytes()).await?;
		stream.write_all(body).await?;
		stream.flush().await?;

		// Same ceiling on the read side as on the connect: the hyper callers
		// pass `Some(READ_TIMEOUT)` to `send`/`read_body`; this path does
		// not, so the wall-clock ceiling has to be put on the head read by
		// hand. The unfixed code waited forever for a peer that opened the
		// handshake and stopped answering.
		let status =
			match tokio::time::timeout(HEAD_READ_TIMEOUT, read_response_head(&mut stream)).await {
				Ok(Ok(s)) => s,
				Ok(Err(e)) => return Err(e),
				Err(_) => {
					return Err(PodmanError::Api {
						status: 0,
						message: format!(
							"exec start response timed out after {} seconds",
							HEAD_READ_TIMEOUT.as_secs()
						),
					});
				}
			};
		if !(200..300).contains(&status) {
			return Err(PodmanError::Api {
				status,
				message: format!("exec start refused with HTTP {status}"),
			});
		}
		Ok(Hijacked { stream })
	}
}

/// Read the response head and return its status code, leaving the socket
/// positioned at the first body byte.
///
/// Reads a byte at a time rather than buffering ahead: a buffered reader would
/// swallow part of the command's output into its own buffer, and that output
/// belongs to the caller. Slow, but the head is a few hundred bytes and it only
/// happens once per exec.
///
/// A peer that never sends the head terminator is drained up to
/// [`MAX_HEAD_BYTES`] and reported with the bytes it actually sent, rather
/// than failing the read at the first byte. That keeps a hostile or buggy
/// daemon from forcing the per-byte rejection path while the bytes it sent
/// stay in the diagnostic.
async fn read_response_head<S: AsyncRead + Unpin>(stream: &mut S) -> Result<u16> {
	use tokio::io::AsyncReadExt;

	let mut head = Vec::with_capacity(256);
	let mut byte = [0u8; 1];
	while !head.ends_with(b"\r\n\r\n") {
		if head.len() >= MAX_HEAD_BYTES {
			return Err(PodmanError::Api {
				status: 0,
				message: format!(
					"exec start response head exceeded its limit ({} bytes read)",
					head.len()
				),
			});
		}
		match stream.read(&mut byte).await? {
			0 => {
				return Err(PodmanError::Api {
					status: 0,
					message: format!(
						"connection closed before the exec start response ({} bytes read)",
						head.len()
					),
				});
			}
			_ => head.push(byte[0]),
		}
	}

	let text = String::from_utf8_lossy(&head);
	let status_line = text.lines().next().unwrap_or_default();
	// `HTTP/1.1 200 OK`: the code is the second token.
	status_line
		.split_whitespace()
		.nth(1)
		.and_then(|c| c.parse::<u16>().ok())
		.ok_or_else(|| PodmanError::Api {
			status: 0,
			message: format!("unparseable exec start response: {status_line:?}"),
		})
}

// The harness listens on a Unix socket; the code under test is the same on
// Windows via the transport enum, whose named-pipe variant CI exercises through
// the client's request path.
#[cfg(all(test, unix))]
#[path = "hijack_tests.rs"]
mod tests;
