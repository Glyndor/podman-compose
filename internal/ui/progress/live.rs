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
use std::time::Instant;

use super::{row, Board};

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
/// arithmetic needs — and the reason every line is truncated to the terminal
/// width first. A line that wraps makes the terminal count two rows where this
/// counted one, and from then on every repaint erases the wrong lines.
pub struct LiveRegion {
	/// Rows painted by the previous repaint, to be walked back over.
	painted: usize,
	/// Terminal width at the last repaint.
	width: usize,
	/// Width of the name column, sized from the seeded rows so it does not jump
	/// as rows come and go.
	name_width: usize,
}

impl LiveRegion {
	/// Start a region, hiding the cursor.
	pub fn new(name_width: usize, width: usize) -> Self {
		let mut err = std::io::stderr();
		let _ = err.write_all(HIDE_CURSOR.as_bytes());
		let _ = err.flush();
		Self {
			painted: 0,
			width,
			name_width,
		}
	}

	/// Re-read the terminal width. Called on each repaint so a resize mid-`up`
	/// does not leave every later line wrapping.
	pub fn refresh_width(&mut self, width: usize) {
		self.width = width;
	}

	/// Walk back over the previous region, print whatever has finished as
	/// permanent scrollback, then paint the rows still in flight.
	pub fn repaint(&mut self, board: &mut Board, frame: usize, now: Instant) {
		let mut out = String::new();
		if self.painted > 0 {
			out.push_str(&cursor_up(self.painted));
		}
		out.push_str(CLEAR_BELOW);

		// Finished rows leave the region and become scrollback. `take_completed_prefix`
		// only releases a contiguous run from the front, so this record stays in
		// the order the work happened.
		for done in board.take_completed_prefix() {
			out.push_str(&row::render(&done, self.name_width, frame, now, self.width));
			out.push('\n');
		}

		let (done, total) = board.tally();
		let mut painted = 0;
		let live = board.live_rows();
		if !live.is_empty() {
			out.push_str(&row::summary(done, total));
			out.push('\n');
			painted += 1;
			for r in live {
				out.push_str(&row::render(r, self.name_width, frame, now, self.width));
				out.push('\n');
				painted += 1;
			}
		}
		self.painted = painted;

		let mut err = std::io::stderr();
		let _ = err.write_all(out.as_bytes());
		let _ = err.flush();
	}

	/// Final repaint with the region emptied, so the last thing on screen is the
	/// permanent record rather than a half-drawn board.
	pub fn finish(&mut self, board: &mut Board, now: Instant) {
		self.repaint(board, 0, now);
	}
}

impl Drop for LiveRegion {
	fn drop(&mut self) {
		let mut err = std::io::stderr();
		let _ = err.write_all(SHOW_CURSOR.as_bytes());
		let _ = err.flush();
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
