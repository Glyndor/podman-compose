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

/// A tail region being repainted on a real terminal.
///
/// Owns the count of rows it last painted, which is the only state the repaint
/// arithmetic needs — and the reason every line handed to it must already be
/// truncated to the terminal width. A line that wraps makes the terminal count
/// two rows where this counted one, and from then on every repaint erases the
/// wrong lines.
///
/// Deliberately knows nothing about what it is drawing. It takes two blocks of
/// finished text: lines that become permanent scrollback, and lines that are
/// erased on the next call. That is what lets the board and `stats` share it —
/// they disagree about everything except needing a block of text repainted in
/// place.
/// Which stream a region draws on.
///
/// Not always stderr. The lifecycle board goes there so stdout stays a clean
/// pipe, but `stats` *is* its output — its table is the thing a user redirects —
/// so its region belongs on stdout. Getting this wrong would put cursor moves in
/// one stream and the content in the other.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Target {
	Stdout,
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

pub struct Region {
	/// Rows painted by the previous repaint, to be walked back over.
	painted: usize,
	target: Target,
}

impl Region {
	/// Start a region on `target`, hiding the cursor.
	pub fn new(target: Target) -> Self {
		target.write(HIDE_CURSOR);
		Self { painted: 0, target }
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

#[cfg(test)]
mod tests {
	use super::*;

	/// The four sequences are what the repaint arithmetic is built on, so they
	/// are pinned rather than left to a typo. `\x1b[3A` moves up three; `\x1b[J`
	/// erases from the cursor down.
	#[test]
	fn the_cursor_moves_are_what_they_claim() {
		assert_eq!(cursor_up(3), "\u{1b}[3A");
		assert_eq!(CLEAR_BELOW, "\u{1b}[J");
		assert_eq!(HIDE_CURSOR, "\u{1b}[?25l");
		assert_eq!(SHOW_CURSOR, "\u{1b}[?25h");
	}
}
