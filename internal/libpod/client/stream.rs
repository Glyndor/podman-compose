//! The platform transport under every libpod connection.
//!
//! Linux and macOS reach Podman over a Unix socket; Windows reaches
//! `podman machine` over a named pipe. Both are byte streams, and everything
//! above this file (the hyper client, the hand-written hijack) only needs
//! `AsyncRead + AsyncWrite`. This enum is the one place that knows which kind
//! of stream a platform uses, so the request path and the hijack path connect
//! through the same code instead of each carrying its own `cfg` blocks.
//!
//! An enum rather than a `Box<dyn ...>` because each build has exactly one
//! variant; the match compiles down to a direct call and the type stays
//! `Debug`-derivable, matching the crate's style.

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::Result;

/// A connected stream to the Podman API: a Unix socket, or a named pipe on
/// Windows.
#[derive(Debug)]
pub(crate) enum SocketStream {
	#[cfg(unix)]
	Unix(tokio::net::UnixStream),
	#[cfg(windows)]
	NamedPipe(tokio::net::windows::named_pipe::NamedPipeClient),
}

impl SocketStream {
	/// Open a new connection to the given socket path (or pipe name).
	#[cfg(unix)]
	pub(crate) async fn connect(socket_path: &str) -> Result<Self> {
		let stream = tokio::net::UnixStream::connect(socket_path)
			.await
			.map_err(|e| super::socket_error(socket_path, e))?;
		Ok(Self::Unix(stream))
	}

	/// Open a new connection to the given socket path (or pipe name).
	///
	/// Named pipes may be momentarily busy (`ERROR_PIPE_BUSY`, os error 231)
	/// while another client's slot frees up; retry a few times before giving
	/// up rather than failing on a transient.
	#[cfg(windows)]
	pub(crate) async fn connect(socket_path: &str) -> Result<Self> {
		use super::PodmanError;

		let mut last_err = None;
		for _ in 0..20 {
			match tokio::net::windows::named_pipe::ClientOptions::new().open(socket_path) {
				Ok(p) => return Ok(Self::NamedPipe(p)),
				Err(e) if e.raw_os_error() == Some(231) => {
					last_err = Some(e);
					tokio::time::sleep(std::time::Duration::from_millis(50)).await;
				}
				Err(e) => return Err(PodmanError::Connect(e)),
			}
		}
		Err(PodmanError::Connect(last_err.unwrap_or_else(|| {
			std::io::Error::new(
				std::io::ErrorKind::TimedOut,
				"named pipe busy after 20 retries",
			)
		})))
	}
}

// Plain delegation: each poll forwards to the one variant this build has. No
// buffering, no translation: the enum exists only to give the two transports
// one type.

impl AsyncRead for SocketStream {
	fn poll_read(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &mut ReadBuf<'_>,
	) -> Poll<std::io::Result<()>> {
		match self.get_mut() {
			#[cfg(unix)]
			Self::Unix(s) => Pin::new(s).poll_read(cx, buf),
			#[cfg(windows)]
			Self::NamedPipe(s) => Pin::new(s).poll_read(cx, buf),
		}
	}
}

impl AsyncWrite for SocketStream {
	fn poll_write(
		self: Pin<&mut Self>,
		cx: &mut Context<'_>,
		buf: &[u8],
	) -> Poll<std::io::Result<usize>> {
		match self.get_mut() {
			#[cfg(unix)]
			Self::Unix(s) => Pin::new(s).poll_write(cx, buf),
			#[cfg(windows)]
			Self::NamedPipe(s) => Pin::new(s).poll_write(cx, buf),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
		match self.get_mut() {
			#[cfg(unix)]
			Self::Unix(s) => Pin::new(s).poll_flush(cx),
			#[cfg(windows)]
			Self::NamedPipe(s) => Pin::new(s).poll_flush(cx),
		}
	}

	fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
		match self.get_mut() {
			#[cfg(unix)]
			Self::Unix(s) => Pin::new(s).poll_shutdown(cx),
			#[cfg(windows)]
			Self::NamedPipe(s) => Pin::new(s).poll_shutdown(cx),
		}
	}
}
