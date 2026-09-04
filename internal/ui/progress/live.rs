//! The tail-region renderer.
//!
//! Completed rows are printed once, as ordinary scrollback, and never touched
//! again. Only the rows still in flight live in the region below them, and only
//! that region is erased and repainted. A tail region rather than an alternate
//! screen, decided by the owner for every command including `stats`: `up` is a
//! command that *finishes*, and taking the screen and handing it back blank
//! destroys the record of what happened.
//!
//! Four escape sequences, no dependency.

use std::io::Write;

/// Hide the cursor. Without it the caret sits at the end of whichever row was
/// painted last and jumps around on every repaint.
const HIDE_CURSOR: &str = "\x1b[?25l";

/// Restore the cursor. Emitted on drop, including the drop that runs while the
/// process is unwinding, so a panic mid-`up` cannot leave the terminal without
/// a caret.
const SHOW_CURSOR: &str = "\x1b[?25h";

/// Erase from the cursor to the end of the screen.
const CLEAR_BELOW: &str = "\x1b[J";

/// Move the cursor up `n` rows.
fn cursor_up(n: usize) -> String {
	format!("\x1b[{n}A")
}

/// Which stream a region draws on.
///
/// Not always stderr. The lifecycle board goes there so stdout stays a clean
/// pipe, but `stats` *is* its output ,  its table is the thing a user redirects ,
/// so its region belongs on stdout. Getting this wrong would put cursor moves in
/// one stream and the content in the other.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
	/// For a region whose content *is* the command's output, so a redirect
	/// captures it. `stats` is the case this exists for.
	Stdout,
	/// For a region that decorates another stream's output. The lifecycle board
	/// goes here so stdout stays a clean pipe.
	Stderr,
}

impl Target {
	fn write(self, text: &str) {
		let mut out: Box<dyn Write> = match self {
			Target::Stdout => Box::new(std::io::stdout()),
			Target::Stderr => Box::new(std::io::stderr()),
		};
		let _ = out.write_all(text.as_bytes());
		let _ = out.flush();
	}
}

/// A tail region being repainted on a real terminal.
///
/// Owns the count of rows it last painted, which is the only state the repaint
/// arithmetic needs ,  and the reason every line handed to it must already be
/// truncated to the terminal width. A line that wraps makes the terminal count
/// two rows where this counted one, and from then on every repaint erases the
/// wrong lines.
///
/// Deliberately knows nothing about what it is drawing. It takes two blocks of
/// finished text: lines that become permanent scrollback, and lines that are
/// erased on the next call. That is what lets the board and `stats` share it ,
/// they disagree about everything except needing a block of text repainted in
/// place.
pub struct Region {
	/// Rows painted by the previous repaint, to be walked back over.
	painted: usize,
	target: Target,
}

impl Region {
	/// Start a region on `target`, hiding the cursor.
	pub fn new(target: Target) -> Self {
		target.write(HIDE_CURSOR);
		restore_cursor_on_interrupt(target);
		Self { painted: 0, target }
	}

	/// Walk the cursor back over the live region and clear below, without
	/// re-writing any rows.
	///
	/// Used by `progress::end` to leave the rows that were in the live region
	/// on screen as the permanent record (the cursor ends below them) without
	/// emitting a second copy of each row. Re-painting via `show(&rows, &[])`
	/// would walk the cursor up, clear, and write the same bytes again, and a
	/// `script -qfc` capture would show each row twice: once from the live
	/// repaint that landed them, once from this final paint that erases and
	/// rewrites them. Erasing without rewriting keeps the rows on screen
	/// exactly once (#1675).
	pub fn close_out(&self) {
		let mut out = String::new();
		if self.painted > 0 {
			out.push_str(&cursor_up(self.painted));
		}
		out.push_str(CLEAR_BELOW);
		self.target.write(&out);
	}

	/// Walk back over the previous region, emit `scrollback` as permanent
	/// history, then paint `live` as the new region.
	///
	/// Both are already-rendered lines: fitting them to the terminal width is
	/// the caller's job, because the caller is the one that knows which cell may
	/// be truncated without losing the point of the row.
	pub fn show(&mut self, scrollback: &[String], live: &[String]) {
		let mut out = String::new();
		if self.painted > 0 {
			out.push_str(&cursor_up(self.painted));
		}
		out.push_str(CLEAR_BELOW);
		for line in scrollback {
			out.push_str(line);
			out.push('\n');
		}
		for line in live {
			out.push_str(line);
			out.push('\n');
		}
		self.painted = live.len();
		self.target.write(&out);
	}
}

impl Drop for Region {
	fn drop(&mut self) {
		self.target.write(SHOW_CURSOR);
	}
}

/// Give the cursor back if the command is interrupted rather than returning.
///
/// `Drop` covers a command that ends on its own, and nothing else: Rust's
/// default SIGINT handling terminates the process without unwinding, so
/// Ctrl-C out of a region left the caret hidden until the user ran `reset`.
/// Measured on a 100x30 pty before the fix ,  one `\e[?25l`, zero `\e[?25h`.
///
/// `stats` is where it bit hardest, because Ctrl-C is the *normal* way to leave
/// it rather than an error path, but a long `up` interrupted part-way had the
/// same hole.
///
/// Exits 130 (128 + SIGINT), the shell convention this binary already documents
/// for an interrupted command. Installed per region rather than globally, so it
/// cannot disturb the commands that handle their own interrupt ,  attached `up`,
/// `exec`, `watch` ,  none of which open one.
fn restore_cursor_on_interrupt(target: Target) {
	if !claim_install() {
		return;
	}
	// A tokio signal task, not a raw handler: the same shape `attach` and
	// `watch` already use, and it runs ordinary code rather than being bound by
	// async-signal-safety.
	tokio::spawn(async move {
		wait_for_interrupt().await;
		target.write(SHOW_CURSOR);
		std::process::exit(130);
	});
}

/// Take the right to install the interrupt handler, once per process.
///
/// Two regions in one invocation ,  a `stats` after an `up` inside an embedding
/// crate ,  must not stack handlers, since each would race to call
/// `process::exit`. Split out of the installer so the latch is testable without
/// spawning anything.
fn claim_install() -> bool {
	static INSTALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
	!INSTALLED.swap(true, std::sync::atomic::Ordering::Relaxed)
}

/// Resolve when the process is asked to stop.
#[cfg(unix)]
async fn wait_for_interrupt() {
	use tokio::signal::unix::{signal, SignalKind};
	let mut term = match signal(SignalKind::terminate()) {
		Ok(s) => s,
		// Without a SIGTERM handler, Ctrl-C alone is still worth catching.
		Err(_) => {
			let _ = tokio::signal::ctrl_c().await;
			return;
		}
	};
	tokio::select! {
		_ = tokio::signal::ctrl_c() => {}
		_ = term.recv() => {}
	}
}

/// Windows has no SIGTERM; Ctrl-C is the interrupt that matters.
#[cfg(not(unix))]
async fn wait_for_interrupt() {
	let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
#[path = "live_tests.rs"]
mod tests;
