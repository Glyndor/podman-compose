//! Test-only fake Podman socket.
//!
//! A minimal libpod HTTP responder bound to a Unix domain socket, so
//! lifecycle/scale/query tests can assert exit-code semantics against canned
//! API responses without a real Podman daemon. [`Client`] opens a fresh
//! connection per request (see `internal/libpod/client/mod.rs`), so this only
//! ever needs to answer one HTTP/1.1 request per accepted connection — no
//! keep-alive.
//!
//! It does frame a chunked body, for one reason: a stream that ends *badly* has
//! a wire shape, and podup's central open question about streaming (#1104) is
//! whether it can tell that shape from a stream that ended well. A real Podman
//! cannot be asked to break a stream on demand, and which shape it produces at a
//! CLEAN end turns out to differ by version — so the two cases are pinned here,
//! deterministically, with no daemon and no version in the picture.

#![cfg(unix)]

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use crate::libpod::Client;

/// What the fake writes back for one request.
pub(super) enum FakeReply {
	/// A `content-length`-framed body, closed cleanly. What every routing test
	/// that only cares about status codes wants.
	Body(u16, String),
	/// A 200 with a **chunked** body: each entry is written as one chunk, then
	/// the terminating zero-length chunk is sent. This is a stream that ends the
	/// way HTTP says it should.
	ChunkedEnd(Vec<String>),
	/// A 200 with a **chunked** body that stops between chunks: each entry is
	/// written as one complete chunk and then the connection is closed with NO
	/// terminating chunk. A stream that died where a chunk header should start.
	ChunkedTruncated(Vec<String>),
	/// A 200 with a **chunked** body cut in the middle of a chunk's payload: the
	/// header promises more bytes than are then written, and the connection
	/// closes. The other place a severed stream can land, and — measured — hyper
	/// classifies the two differently, which is why both exist here.
	ChunkedCutMidPayload(String),
	/// The request is read and accepted, and then the connection closes with **no
	/// response at all** — not even a status line.
	///
	/// This is the shape `PodmanError::is_incomplete_message` names: hyper's
	/// `IncompleteMessage` is about the message *head*, so it is the one reply
	/// here that produces it, and the severed-body variants above do not.
	///
	/// It is what libpod does on Podman 6 to the container-archive PUT (#1097,
	/// applies the archive then hangs up) and to state-changing POSTs under
	/// concurrency (#1339). Both are handled by re-checking the observable out of
	/// band, and until this existed neither discriminator had a test that could
	/// reach it.
	ClosedWithoutResponse,
}

/// A test's routing rule: `(method, target) -> reply`, where `target` is the
/// request path plus its raw query string (e.g.
/// `/v5.0.0/libpod/containers/proj-web-1/start`).
type Responder = dyn Fn(&str, &str) -> FakeReply + Send + Sync;

/// A fake libpod socket driven by a routing closure; see [`Responder`].
pub(super) struct FakePodman {
	sock_path: std::path::PathBuf,
	/// Every request answered so far, as `"METHOD target"` — lets a test assert
	/// that a best-effort pass attempted every container even after one of them
	/// failed.
	pub(super) requests: Arc<Mutex<Vec<String>>>,
	_dir: tempfile::TempDir,
	task: JoinHandle<()>,
}

impl FakePodman {
	/// A fresh [`Client`] pointed at this fake socket. [`Client`] is a thin,
	/// stateless per-request handle (see `internal/libpod/client/mod.rs`), so a
	/// new one is created on every call rather than shared/cloned.
	pub(super) fn client(&self) -> Client {
		Client::new(self.sock_path.to_string_lossy().into_owned())
	}
}

impl Drop for FakePodman {
	fn drop(&mut self) {
		// Stop accepting new connections; in-flight ones simply finish or are
		// dropped along with the temp dir (the test is already done with them).
		self.task.abort();
	}
}

/// Start a fake Podman socket that answers every request via `respond`.
/// Connection-per-request, matching how [`Client`] talks to the real daemon.
pub(super) fn start<F>(respond: F) -> FakePodman
where
	F: Fn(&str, &str) -> (u16, String) + Send + Sync + 'static,
{
	start_replying(move |method, target| {
		let (status, body) = respond(method, target);
		FakeReply::Body(status, body)
	})
}

/// As [`start`], but the routing closure chooses the wire shape too — including
/// a chunked body that ends cleanly or one that is cut off mid-stream.
pub(super) fn start_replying<F>(respond: F) -> FakePodman
where
	F: Fn(&str, &str) -> FakeReply + Send + Sync + 'static,
{
	let dir = tempfile::tempdir().expect("create temp dir for fake podman socket");
	let sock_path = dir.path().join("podman.sock");
	let listener = UnixListener::bind(&sock_path).expect("bind fake podman socket");
	let respond: Arc<Responder> = Arc::new(respond);
	let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

	let task_requests = requests.clone();
	let task = tokio::spawn(async move {
		loop {
			let Ok((stream, _)) = listener.accept().await else {
				break;
			};
			let respond = respond.clone();
			let requests = task_requests.clone();
			tokio::spawn(async move {
				let _ = serve_one(stream, respond.as_ref(), &requests).await;
			});
		}
	});

	FakePodman {
		sock_path,
		requests,
		_dir: dir,
		task,
	}
}

/// Read one HTTP/1.1 request line (every request this harness serves carries
/// an empty body, so the header block is the whole request) and write back
/// the canned response.
async fn serve_one(
	mut stream: UnixStream,
	respond: &Responder,
	requests: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
	let mut buf = Vec::new();
	let mut chunk = [0u8; 1024];
	loop {
		let n = stream.read(&mut chunk).await?;
		if n == 0 {
			break;
		}
		buf.extend_from_slice(&chunk[..n]);
		if buf.windows(4).any(|w| w == b"\r\n\r\n") {
			break;
		}
	}

	let head = String::from_utf8_lossy(&buf);
	let request_line = head.lines().next().unwrap_or_default();
	let mut parts = request_line.split_whitespace();
	let method = parts.next().unwrap_or_default().to_string();
	let target = parts.next().unwrap_or_default().to_string();

	requests.lock().unwrap().push(format!("{method} {target}"));

	match respond(&method, &target) {
		FakeReply::Body(status, body) => {
			let response = format!(
				"HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n\r\n{body}",
				reason = reason_phrase(status),
				len = body.len(),
			);
			stream.write_all(response.as_bytes()).await?;
		}
		FakeReply::ChunkedEnd(chunks) => {
			write_chunked(&mut stream, &chunks).await?;
			// The terminating zero-length chunk: this is the difference between
			// a finished body and a severed one, and it is the whole subject of
			// #1104.
			stream.write_all(b"0\r\n\r\n").await?;
		}
		FakeReply::ChunkedTruncated(chunks) => {
			write_chunked(&mut stream, &chunks).await?;
			// No terminator. Closing here is what a dead stream looks like.
		}
		FakeReply::ChunkedCutMidPayload(chunk) => {
			write_chunked(&mut stream, &[]).await?;
			// Promise the whole chunk, deliver half of it, hang up.
			let half = chunk.len() / 2;
			stream
				.write_all(format!("{:x}\r\n{}", chunk.len(), &chunk[..half]).as_bytes())
				.await?;
			stream.flush().await?;
		}
		FakeReply::ClosedWithoutResponse => {
			// Write nothing. The shutdown below is the entire reply.
		}
	}
	stream.shutdown().await?;
	Ok(())
}

/// Reason phrase for the statuses this harness's tests use; anything else
/// falls back to a placeholder (the client only parses the numeric code).
fn reason_phrase(status: u16) -> &'static str {
	match status {
		200 => "OK",
		204 => "No Content",
		304 => "Not Modified",
		404 => "Not Found",
		409 => "Conflict",
		500 => "Internal Server Error",
		_ => "Unknown",
	}
}

/// Write a chunked-encoding response head and one chunk per entry, leaving the
/// caller to decide whether the terminating chunk follows.
async fn write_chunked(stream: &mut UnixStream, chunks: &[String]) -> std::io::Result<()> {
	stream
		.write_all(
			b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
		)
		.await?;
	for chunk in chunks {
		stream
			.write_all(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes())
			.await?;
	}
	stream.flush().await
}
