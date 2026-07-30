//! The live board: what a lifecycle command is working through, and where it
//! has got to.
//!
//! One model, two renderers. Which one runs is decided from the terminal and the
//! colour choice, never from the command: `up` on a tty repaints a tail region,
//! `up` in a pipe emits the same events as plain append-only lines. **Animation
//! in a CI log is a defect**, and so is a CI log that says less than the
//! terminal did — both renderers see every event, including the intermediate
//! transitions the old output dropped entirely.
//!
//! Everything goes to stderr. stdout stays a clean pipe, which is what lets
//! `run -d` keep printing its container id there.

use std::sync::Mutex;
use std::time::Instant;

mod board;
mod live;
mod row;

pub use board::{Board, Kind, Row, State};
pub use live::{Region, Target};

/// How often the region repaints while nothing is happening, so the spinner
/// turns during a long pull instead of looking like a hang. The same cadence
/// `terminal::windows`'s resize poll already uses.
const TICK: std::time::Duration = std::time::Duration::from_millis(100);

/// The board for the command currently running, if one asked for it.
///
/// Process-global for the same reason the colour choice is: one CLI invocation
/// runs one lifecycle command, and threading a handle through every engine call
/// site would change 21 signatures to say something none of them decides. A
/// library embedder never gets one — [`begin`] is a no-op unless the CLI turned
/// progress on.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// The repaint ticker, kept so it can be stopped when the board ends.
static TICKER: Mutex<Option<tokio::task::JoinHandle<()>>> = Mutex::new(None);

struct Session {
	board: Board,
	/// `None` when the sink is plain: not a terminal, colour off, or the
	/// terminal size could not be read.
	region: Option<live::Region>,
	frame: usize,
	/// Width of the name column, sized from the seeded rows so it does not jump
	/// as rows come and go.
	name_width: usize,
}

/// Whether a live region is allowed right now.
///
/// Three conditions, all required. stderr must be a terminal — `--ansi always |
/// tee` is a log, not a terminal, and repainting into it writes cursor moves
/// into a file. The colour choice must not be `Never`, because someone who asked
/// for no escapes means it. And the width must be readable, since every line is
/// truncated to it and the repaint arithmetic depends on that truncation.
fn live_allowed() -> Option<usize> {
	use std::io::IsTerminal;
	if !std::io::stderr().is_terminal() || !super::stderr_colored() {
		return None;
	}
	width_from(crate::engine::query::terminal::window_size())
}

/// The usable width from a `window_size()` answer.
///
/// Split out so the one thing that can silently be wrong here is testable:
/// `window_size` returns **`(rows, cols)`**, in that order, and reading the
/// first element as the width truncated every board line to the terminal's
/// *height* instead. Measured on a 100x30 pty, every verb came out as `Pen…`,
/// and nothing in the type system or the suite objected.
fn width_from(size: Option<(u16, u16)>) -> Option<usize> {
	size.map(|(_rows, cols)| cols as usize)
}

/// Start a board over `resources`, in the order they will be worked through.
///
/// A no-op when progress output is off, so an embedding crate never gets a
/// board it did not ask for.
pub fn begin(resources: impl IntoIterator<Item = (Kind, String)>) {
	if !super::progress_enabled() {
		return;
	}
	let board = Board::new(resources);
	let name_width = board
		.live_rows()
		.iter()
		.map(|r| r.name.chars().count())
		.max()
		.unwrap_or(0)
		.clamp(12, 40);
	let region = live_allowed().map(|_| live::Region::new(live::Target::Stderr));
	let live = region.is_some();
	if let Ok(mut slot) = SESSION.lock() {
		*slot = Some(Session {
			board,
			region,
			frame: 0,
			name_width,
		});
	}
	if live {
		spawn_ticker();
	}
	repaint();
}

/// Report that work on a resource has begun.
///
/// This is the event the tree could not previously produce: every one of the 21
/// existing progress sites fires once the work is already over.
pub fn start(kind: &str, name: &str, verb: &str) {
	let Some(kind) = Kind::from_noun(kind) else {
		return;
	};
	let sink = {
		let Ok(mut slot) = SESSION.lock() else {
			return;
		};
		match slot.as_mut() {
			Some(session) => {
				session.board.start(kind, name, verb, Instant::now());
				if session.region.is_some() {
					Sink::Live
				} else {
					Sink::Plain
				}
			}
			None => Sink::None,
		}
	};
	emit(sink, kind, name, verb);
}

/// Which renderer a just-recorded event belongs to.
enum Sink {
	/// A region is open; the event shows up on the next repaint.
	Live,
	/// A board is open but not on a terminal, so the event is a line.
	Plain,
	/// No board at all.
	None,
}

/// Hand a recorded event to whichever renderer owns it.
///
/// The plain sink writes the same line the tree has always written, through
/// [`super::write_progress_line`] rather than `progress_line` — going back
/// through the routing that sent the event here would be a loop, and it is what
/// made a piped `up` print nothing at all the first time this was wired up: the
/// board swallowed every line and no renderer put one back.
fn emit(sink: Sink, kind: Kind, name: &str, verb: &str) {
	match sink {
		Sink::Live => repaint(),
		Sink::Plain => super::write_progress_line(kind.noun(), name, verb),
		// No board open, but the caller still asked to report a transition, so
		// the line is still owed — this is the "a pipe says more than it used
		// to, never less" half of the contract.
		Sink::None => {
			if super::progress_enabled() {
				super::write_progress_line(kind.noun(), name, verb);
			}
		}
	}
}

/// Report that work on a resource has finished. Returns whether a board took it;
/// `false` leaves the caller to print its own line, which is what keeps every
/// existing `progress_line` call site working unchanged.
pub(super) fn finish(kind: &str, name: &str, verb: &str) -> bool {
	let Some(kind) = Kind::from_noun(kind) else {
		return false;
	};
	let sink = {
		let Ok(mut slot) = SESSION.lock() else {
			return false;
		};
		match slot.as_mut() {
			Some(session) => {
				session.board.finish(kind, name, verb, Instant::now());
				if session.region.is_some() {
					Sink::Live
				} else {
					Sink::Plain
				}
			}
			None => Sink::None,
		}
	};
	// `Sink::None` means no board took it, so the caller prints its own line as
	// it always has. The other two mean the event is accounted for here.
	if matches!(sink, Sink::None) {
		return false;
	}
	emit(sink, kind, name, verb);
	true
}

/// Close the board: stop the ticker, draw the finished state, restore the
/// cursor.
///
/// Idempotent, because the commands that open a board have several exits.
pub fn end() {
	if let Ok(mut slot) = TICKER.lock() {
		if let Some(handle) = slot.take() {
			handle.abort();
		}
	}
	let Ok(mut slot) = SESSION.lock() else {
		return;
	};
	if let Some(mut session) = slot.take() {
		// One last paint with the region emptied, so the last thing on screen is
		// the permanent record rather than a half-drawn board.
		if session.region.is_some() {
			let width = live_allowed().unwrap_or(0);
			let now = Instant::now();
			let scrollback: Vec<String> = session
				.board
				.take_completed_prefix()
				.iter()
				.map(|r| row::render(r, session.name_width, 0, now, width))
				.collect();
			let leftover: Vec<String> = session
				.board
				.live_rows()
				.iter()
				.map(|r| row::render(r, session.name_width, 0, now, width))
				.collect();
			if let Some(region) = session.region.as_mut() {
				region.show(&scrollback, &leftover);
			}
		}
	}
}

/// Repaint the region, if there is one. A plain sink does nothing here: its
/// lines are emitted by the event calls themselves.
fn repaint() {
	let Ok(mut slot) = SESSION.lock() else {
		return;
	};
	let Some(session) = slot.as_mut() else {
		return;
	};
	let frame = session.frame;
	let name_width = session.name_width;
	let Session { board, region, .. } = session;
	let Some(region) = region.as_mut() else {
		return;
	};
	// Re-read the width every repaint, so a resize mid-command does not leave
	// every later line wrapping — and wrapping is what breaks the arithmetic.
	let width = live_allowed().unwrap_or(0);
	let now = Instant::now();
	let scrollback: Vec<String> = board
		.take_completed_prefix()
		.iter()
		.map(|r| row::render(r, name_width, frame, now, width))
		.collect();
	let mut lines = Vec::new();
	let rows = board.live_rows();
	if !rows.is_empty() {
		let (done, total) = board.tally();
		lines.push(row::summary(done, total));
		lines.extend(
			rows.iter()
				.map(|r| row::render(r, name_width, frame, now, width)),
		);
	}
	region.show(&scrollback, &lines);
}

/// Advance the spinner and repaint, so a long pull turns rather than freezing.
fn spawn_ticker() {
	let handle = tokio::spawn(async {
		let mut interval = tokio::time::interval(TICK);
		loop {
			interval.tick().await;
			{
				let Ok(mut slot) = SESSION.lock() else { return };
				match slot.as_mut() {
					Some(session) => session.frame = session.frame.wrapping_add(1),
					None => return,
				}
			}
			repaint();
		}
	});
	if let Ok(mut slot) = TICKER.lock() {
		*slot = Some(handle);
	}
}

#[cfg(test)]
mod tests {
	use super::width_from;

	/// `window_size` answers `(rows, cols)`. Getting this backwards is invisible
	/// on a square-ish terminal and mangles every line on a normal one.
	#[test]
	fn the_width_is_the_columns_not_the_rows() {
		assert_eq!(width_from(Some((30, 100))), Some(100));
		assert_eq!(width_from(None), None);
	}
}
