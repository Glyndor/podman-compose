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
	region: Option<live::LiveRegion>,
	frame: usize,
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
	crate::engine::query::terminal::window_size().map(|(cols, _)| cols as usize)
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
	let region = live_allowed().map(|width| live::LiveRegion::new(name_width, width));
	let live = region.is_some();
	if let Ok(mut slot) = SESSION.lock() {
		*slot = Some(Session {
			board,
			region,
			frame: 0,
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
	let handled = {
		let Ok(mut slot) = SESSION.lock() else {
			return;
		};
		match slot.as_mut() {
			Some(session) => {
				session.board.start(kind, name, verb, Instant::now());
				true
			}
			None => false,
		}
	};
	if handled {
		repaint();
	} else if super::progress_enabled() {
		// No board: still emit the transition, so a plain sink says as much as a
		// live one. This is the "more information than today" half of the
		// contract, and it must not depend on a board being open.
		super::progress_line(kind.noun(), name, verb);
	}
}

/// Report that work on a resource has finished. Returns whether a board took it;
/// `false` leaves the caller to print its own line, which is what keeps every
/// existing `progress_line` call site working unchanged.
pub(super) fn finish(kind: &str, name: &str, verb: &str) -> bool {
	let Some(kind) = Kind::from_noun(kind) else {
		return false;
	};
	let handled = {
		let Ok(mut slot) = SESSION.lock() else {
			return false;
		};
		match slot.as_mut() {
			Some(session) => {
				session.board.finish(kind, name, verb, Instant::now());
				true
			}
			None => false,
		}
	};
	if handled {
		repaint();
	}
	handled
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
		if let Some(region) = session.region.as_mut() {
			region.finish(&mut session.board, Instant::now());
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
	let Session { board, region, .. } = session;
	if let Some(region) = region.as_mut() {
		if let Some(width) = live_allowed() {
			region.refresh_width(width);
		}
		region.repaint(board, frame, Instant::now());
	}
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
