//! The bidirectional terminal loop, shared by interactive `exec` and `run`.
//!
//! Both hand a hijacked socket to the caller's terminal and pump bytes until the
//! command ends. They differ only in which libpod object gets resized ,  an exec
//! session or a container ,  so that is the one parameter, and the loop that must
//! not get raw mode wrong lives once rather than twice.
//!
//! The loop itself is platform-independent: raw mode, the size query and the
//! resize events differ per platform, and all three live behind the one
//! signature `query::terminal` exports.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{ComposeError, Result};
use crate::libpod::client::Hijacked;
use crate::libpod::API_PREFIX;

use super::query::terminal::{drain_stdin, window_size, RawMode, ResizeWatcher};
use super::Engine;

impl Engine {
	/// Pump the caller's terminal against a hijacked stream until it ends.
	///
	/// `resize_base` is the libpod path segment identifying what to resize ,
	/// `exec/{id}` or `containers/{name}` ,  since the endpoint differs and
	/// nothing else does.
	///
	/// The terminal is restored by `RawMode`'s `Drop`, so every exit path leaves
	/// the caller's shell usable. That is the one thing this must not get wrong:
	/// a terminal left raw has no echo and no line discipline, and the user
	/// cannot see what they type to fix it.
	pub(crate) async fn pump_terminal(&self, hijacked: Hijacked, resize_base: &str) -> Result<()> {
		// Raw mode only *after* the stream is known to be live. Enabling it
		// first would leave a terminal unusable behind a failed start.
		let _raw = RawMode::enable();

		// Size the pty now, not before: there is nothing to resize until the
		// session exists, and libpod rejects the call ,  silently, since a failed
		// resize is only cosmetic. Sizing here means a full-screen program draws
		// correctly from its first frame rather than at libpod's 80x24 default.
		if let Some((rows, cols)) = window_size() {
			self.resize_pty(resize_base, rows, cols).await;
		}

		// Drop any bytes the kernel had queued on stdin before raw mode took
		// effect. Switching to raw surfaces them immediately to a read, and a
		// pty-startup NUL has been measured to land here under `script -qfc`
		// (see #1675): forwarding it would echo back as `^@` from the remote
		// pty and pollute the output as the first byte. The caller has not
		// had a chance to type anything yet, so any byte read here is pty
		// startup and is safe to drop.
		drain_stdin();

		let (mut server_read, mut server_write) = tokio::io::split(hijacked.stream);
		let mut stdin = tokio::io::stdin();
		let mut stdout = tokio::io::stdout();

		// Follow the window. libpod has no way to learn the caller's terminal
		// size on its own, so every change has to be forwarded or a resized
		// window leaves the remote program drawing at the old geometry.
		let mut resize = ResizeWatcher::new().map_err(ComposeError::Io)?;

		let mut to_server = [0u8; 8 * 1024];
		let mut from_server = [0u8; 32 * 1024];

		loop {
			tokio::select! {
				// Output first: a biased select would starve it behind a caller
				// holding the keyboard down, and seeing the command's output is
				// the point of being attached.
				read = server_read.read(&mut from_server) => {
					match read {
						Ok(0) => break,
						Ok(n) => {
							stdout.write_all(&from_server[..n]).await.map_err(ComposeError::Io)?;
							stdout.flush().await.map_err(ComposeError::Io)?;
						}
						// The command exiting closes this side; that is the
						// ordinary end of an interactive session, not a failure.
						Err(_) => break,
					}
				}
				read = stdin.read(&mut to_server) => {
					match read {
						// Ctrl-D / a closed stdin: stop writing but keep reading,
						// so the command can finish and flush. Shutting the write
						// half is what tells it EOF.
						Ok(0) => {
							let _ = server_write.shutdown().await;
						}
						Ok(n) => {
							if server_write.write_all(&to_server[..n]).await.is_err() {
								break;
							}
							if server_write.flush().await.is_err() {
								break;
							}
						}
						Err(_) => break,
					}
				}
				size = resize.next() => {
					let (rows, cols) = size;
					self.resize_pty(resize_base, rows, cols).await;
				}
			}
		}

		Ok(())
	}

	/// Tell libpod the pty's new size. Best-effort: a failed resize makes the
	/// remote program draw at the wrong geometry, which is cosmetic, and turning
	/// it into a hard error would kill a working session over a window drag.
	async fn resize_pty(&self, resize_base: &str, rows: u16, cols: u16) {
		let path = format!("{API_PREFIX}/{resize_base}/resize?h={rows}&w={cols}");
		if let Err(e) = self.client.post_empty_ok(&path).await {
			tracing::debug!("resize to {rows}x{cols} failed: {e}");
		}
	}
}
