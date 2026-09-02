//! The termios implementation of the terminal contract — see the module doc
//! in `mod.rs` for the contract and the restore-on-drop invariant.

// termios and the winsize ioctl are libc FFI. The crate denies `unsafe` and
// modules that need it opt back in locally, with a soundness comment per block —
// see `engine::lock` and `engine::staging` for the same pattern.
#![allow(unsafe_code)]

use std::os::fd::AsRawFd;

/// A terminal switched to raw mode, restored on drop.
///
/// Holding the original `termios` rather than reconstructing a "sane" one
/// matters: the caller's shell may have its own settings, and putting the
/// terminal into what podup thinks is normal would quietly change them.
pub(crate) struct RawMode {
	fd: i32,
	original: libc::termios,
}

impl RawMode {
	/// Switch stdin to raw mode, or return `None` when it is not a terminal.
	///
	/// Not being a TTY is the ordinary case for `podup exec` in a script or a
	/// pipeline, not an error: there is no line discipline to disable, and the
	/// caller streams bytes as before.
	pub(crate) fn enable() -> Option<Self> {
		Self::enable_on(std::io::stdin().as_raw_fd())
	}

	/// The same, on an explicit descriptor.
	///
	/// `enable` is the only place that consults the ambient stdin, which keeps
	/// this testable: a test that asserted on `enable()` directly would be
	/// asserting that whoever runs `cargo test` has no terminal — true under a
	/// pipe, false in a shell, and on the way to being false it would put the
	/// developer's own terminal into raw mode.
	pub(super) fn enable_on(fd: i32) -> Option<Self> {
		// SAFETY: `isatty` only inspects the descriptor and cannot write through
		// it; a non-terminal descriptor returns 0 rather than misbehaving.
		if unsafe { libc::isatty(fd) } != 1 {
			return None;
		}

		let mut original: libc::termios = unsafe { std::mem::zeroed() };
		// SAFETY: `original` is a correctly sized, zeroed `termios` owned here,
		// and `fd` is a terminal (checked above). `tcgetattr` only writes into it.
		if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
			return None;
		}

		let mut raw = original;
		// SAFETY: `cfmakeraw` only mutates the struct it is given.
		unsafe { libc::cfmakeraw(&mut raw) };
		// SAFETY: `raw` is a fully initialized `termios` derived from the current
		// settings. TCSANOW applies immediately, which is what an interactive
		// session wants — a drained flush would swallow keystrokes already typed.
		if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
			return None;
		}

		Some(Self { fd, original })
	}
}

impl Drop for RawMode {
	fn drop(&mut self) {
		// SAFETY: `self.original` is the exact `termios` read from this same
		// descriptor in `enable`, so restoring it cannot leave the terminal in a
		// state it was not already in. Errors are ignored because there is
		// nothing useful to do while unwinding, and reporting one would replace
		// the real error the caller is already handling.
		unsafe {
			libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
		}
	}
}

/// The terminal's current size as `(rows, cols)`, or `None` when no descriptor
/// can answer.
///
/// Used to size the remote pty at start and to follow it on `SIGWINCH`: without
/// this, a full-screen program inside the container draws to an 80x24 default
/// and redraws wrong the moment the window changes.
///
/// **stdin first, then stdout.** Asking only stdout looks natural — that is
/// where the drawing goes — but the two can differ: `podup exec -it db psql >
/// out.txt` types into a terminal and writes to a file, and stdout then answers
/// "not a terminal" while a perfectly good size sits on stdin. Interactivity is
/// decided by stdin, so the size is asked of stdin too, and stdout is the
/// fallback for the reverse case.
pub(crate) fn window_size() -> Option<(u16, u16)> {
	size_of(std::io::stdin().as_raw_fd()).or_else(|| size_of(std::io::stdout().as_raw_fd()))
}

/// `TIOCGWINSZ` on one descriptor.
fn size_of(fd: i32) -> Option<(u16, u16)> {
	let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
	// SAFETY: `ws` is a correctly sized, zeroed `winsize` owned here, and
	// TIOCGWINSZ only writes into it. A non-terminal descriptor returns an error
	// rather than writing garbage.
	if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
		return None;
	}
	// A terminal that reports 0x0 has no usable geometry — treat it as unknown
	// rather than sizing the remote pty to nothing.
	if ws.ws_row == 0 || ws.ws_col == 0 {
		return None;
	}
	Some((ws.ws_row, ws.ws_col))
}

/// Resize events for the interactive pump: yields a size to apply whenever the
/// caller's window changes.
///
/// On Unix the kernel says when: each `SIGWINCH` is followed by a fresh
/// [`window_size`] query, so the size handed out is the one current at
/// delivery, not at registration.
pub(crate) struct ResizeWatcher {
	signal: tokio::signal::unix::Signal,
}

impl ResizeWatcher {
	/// Register for window-change signals.
	pub(crate) fn new() -> std::io::Result<Self> {
		Ok(Self {
			signal: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?,
		})
	}

	/// The next size to apply.
	///
	/// Pends forever once no further signal can arrive, so a `select!` arm on
	/// it goes quiet rather than spinning on a closed stream; and a signal
	/// whose size cannot be read is skipped, since there is nothing to apply.
	pub(crate) async fn next(&mut self) -> (u16, u16) {
		loop {
			if self.signal.recv().await.is_none() {
				std::future::pending::<()>().await;
			}
			if let Some(size) = window_size() {
				return size;
			}
		}
	}
}

#[cfg(test)]
#[path = "unix_tests.rs"]
mod tests;
