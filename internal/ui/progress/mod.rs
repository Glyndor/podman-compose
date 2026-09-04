//! The live board: what a lifecycle command is working through, and where it
//! has got to.
//!
//! One model, two renderers. Which one runs is decided from the terminal and the
//! colour choice, never from the command: `up` on a tty repaints a tail region,
//! `up` in a pipe emits the same events as plain append-only lines. **Animation
//! in a CI log is a defect**, and so is a CI log that says less than the
//! terminal did ,  both renderers see every event, including the intermediate
//! transitions the old output dropped entirely.
//!
//! Everything goes to stderr. stdout stays a clean pipe, which is what lets
//! `run -d` keep printing its container id there.

use std::sync::Mutex;
use std::time::Instant;

mod board;
#[cfg(test)]
pub(crate) mod capture;
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
/// library embedder never gets one ,  [`begin`] is a no-op unless the CLI turned
/// progress on.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Whether a board is currently open, independent of [`SESSION`]. Set by
/// [`begin`] and cleared by [`end`]; consulted by [`finish`] (which
/// `progress_line` routes through) so the common "no board" path short-circuits
/// the `Mutex::lock` without taking it. Uncontended `Mutex::lock` is ~20 ns but
/// `progress_line` fires once per resource per command, so over a 100-service
/// `up` the lock+unlock pairs add up to ~2 µs of pure syscall work (#1364).
static SESSION_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

/// Whether the live tail region is allowed at all.
///
/// The decision depends on the terminal only: the spinner and the in-place
/// repaint are animations the terminal drives, and a log file is no terminal.
/// `is_terminal()` answers that without consulting the colour choice, so
/// NO_COLOR=1 and `--ansi never` no longer collapse the live board into a
/// stream of plain lines (#1672): a styled run that asked for no escapes keeps
/// its animation, and the styling is stripped at the renderer instead of the
/// renderer being skipped.
///
/// `super::stderr_colored()` is consulted separately, inside
/// [`row::render`], so a single board colour can be on or off without the
/// region being on or off.
fn live_terminal() -> bool {
	use std::io::IsTerminal;
	std::io::stderr().is_terminal()
}

/// The current terminal width from a `window_size()` answer, or `None` when
/// the runtime cannot tell. Per-repaint so a resize mid-command is reflected
/// on the next frame (#1364).
fn live_width() -> Option<usize> {
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
	// Recorded before the gate, so a test can read which rows a command asked
	// for without switching the process-wide progress flag on for every other
	// test in the suite. Compiled out of a release build entirely.
	#[cfg(test)]
	let resources: Vec<(Kind, String)> = resources.into_iter().collect();
	#[cfg(test)]
	capture::record_begin(&resources, live_terminal());
	if !super::progress_enabled() {
		return;
	}
	// Cache the terminal+colour decision here (it cannot change mid-command)
	// and only re-read the width per repaint, so a resize mid-command is
	// still honoured. `live_allowed` previously re-ran both halves ten times a
	// second for the lifetime of the board (#1364).
	let live = live_terminal();
	let board = Board::new(resources);
	let name_width = board
		.live_rows()
		.iter()
		.map(|r| r.name.chars().count())
		.max()
		.unwrap_or(0)
		.clamp(12, 40);
	let region = live.then(|| live::Region::new(live::Target::Stderr));
	if let Ok(mut slot) = SESSION.lock() {
		*slot = Some(Session {
			board,
			region,
			frame: 0,
			name_width,
		});
	}
	// The flag goes up *after* the session is installed: a `finish` racing the
	// install sees either no session (`SESSION_OPEN` false → no lock taken) or
	// a fully-built session. The reverse order would let `finish` take the
	// lock only to find an empty session and return false ,  same result, more
	// work.
	SESSION_OPEN.store(true, std::sync::atomic::Ordering::Release);
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

/// The plain-sink buffer for transitional verbs that have not yet been
/// replaced by a final one.
///
/// A pipe or CI log prints one line per resource, with the final verb. The
/// transitional verb (`Creating`, `Starting`, `Removing`, `Pulling`, …) is
/// buffered until the final one arrives; if no final arrives (a crash mid-way)
/// the buffered verb is flushed at `progress::end` so the log still records
/// what was in flight (#1673).
///
/// Process-global for the same reason the board is: one CLI invocation runs one
/// lifecycle command, and threading a buffer through every engine call site
/// would change 21 signatures to say something none of them decides.
static PLAIN_BUFFER: Mutex<Vec<(Kind, String, String)>> = Mutex::new(Vec::new());

/// Whether `verb` looks transitional: present participle in `-ing`.
///
/// The plain sink buffers these until a final verb arrives for the same
/// `(kind, name)`, so a log shows one line per resource instead of a
/// contradicting pair like `Network x Creating / Network x Exists` (#1673).
///
/// The `-ning` exception matters: `Running` is a final state that ends in
/// `-ing` and would otherwise be buffered, and either flushed at end (which
/// would print it twice ,  once when the row closes, once when the buffer
/// drains) or held forever (which would lose it on a clean exit). Verbs the
/// call sites actually pass through `progress::start` are the present
/// participles `Creating`, `Starting`, `Stopping`, `Removing`, `Pulling`,
/// `Pushing`, `Recreating` ,  `Running` reaches the final path through
/// `progress_line`, not `progress::start`.
fn is_transitional(verb: &str) -> bool {
	let verb = verb.trim();
	if verb.len() < 4 {
		return false;
	}
	let lower = verb.to_ascii_lowercase();
	if !lower.ends_with("ing") {
		return false;
	}
	// `Running` and other `-ning` finals are not transitional.
	if lower.ends_with("ning") {
		return false;
	}
	// `Missing` is a noun-form used as a status word, not a transitional.
	lower != "missing"
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
/// [`super::write_progress_line`] rather than `progress_line` ,  going back
/// through the routing that sent the event here would be a loop, and it is what
/// made a piped `up` print nothing at all the first time this was wired up: the
/// board swallowed every line and no renderer put one back.
///
/// A transitional verb on the plain sink is buffered, not written: the log
/// contract is one line per resource with the final verb, and writing the
/// transitional one first would force the writer to overwrite or amend it
/// when the final arrived, which a pipe cannot do. The final verb flushes the
/// buffered one (drops it) and writes itself. `progress::end` flushes any
/// still-buffered entries so a crash mid-way still leaves `Creating` in the
/// log (#1673).
fn emit(sink: Sink, kind: Kind, name: &str, verb: &str) {
	match sink {
		Sink::Live => repaint(),
		// Both `Plain` (board open but no region) and `None` (no board at all)
		// go to a log-style line, and the log-style contract is the same in both:
		// one line per resource with the final verb. A `run --rm` that pre-creates
		// networks reaches this branch through `Sink::None` because no board is
		// ever opened, and it must still suppress the transitional verb (#1673).
		Sink::Plain | Sink::None => {
			if !super::progress_enabled() {
				return;
			}
			if is_transitional(verb) {
				buffer_push(kind, name, verb);
			} else {
				// A final verb that says nothing happened (`Exists`, `Running`,
				// `Absent`, `Skipped`) makes the transitional line before it a
				// contradiction, so it is dropped; a final verb that reports
				// work keeps the transitional line above it, so a log still
				// shows when the work started.
				if let Some(doing) = buffer_take(kind, name) {
					if !is_noop_final(verb) {
						super::write_progress_line(kind.noun(), name, &doing);
					}
				}
				super::write_progress_line(kind.noun(), name, verb);
			}
		}
	}
}

/// A final verb after which nothing was done: the resource was already in
/// the state the command wanted, or was not there to act on.
fn is_noop_final(verb: &str) -> bool {
	matches!(verb.trim(), "Exists" | "Running" | "Absent" | "Skipped")
}

/// Remove and return the transitional verb buffered for `(kind, name)`.
fn buffer_take(kind: Kind, name: &str) -> Option<String> {
	let mut buf = PLAIN_BUFFER.lock().ok()?;
	let pos = buf
		.iter()
		.position(|entry| entry.0 == kind && entry.1 == name)?;
	Some(buf.remove(pos).2)
}

/// Append `(kind, name, verb)` to the plain-sink buffer.
fn buffer_push(kind: Kind, name: &str, verb: &str) {
	if let Ok(mut buf) = PLAIN_BUFFER.lock() {
		// A second transitional for the same (kind, name) replaces the first
		// rather than queuing two: a `Recreating` then `Stopping` is reported
		// as the latter, which is the more recent fact.
		buf.retain(|entry| !(entry.0 == kind && entry.1 == name));
		buf.push((kind, name.to_string(), verb.to_string()));
	}
}

/// Drain the buffer in insertion order, emitting one line per entry. Used at
/// `progress::end` to flush transitional verbs that never received a final.
fn buffer_drain() {
	let drained: Vec<(Kind, String, String)> = if let Ok(mut buf) = PLAIN_BUFFER.lock() {
		std::mem::take(&mut *buf)
	} else {
		Vec::new()
	};
	for (kind, name, verb) in drained {
		super::write_progress_line(kind.noun(), &name, &verb);
	}
}

#[cfg(test)]
pub(crate) fn buffer_drain_for_tests() {
	buffer_drain();
}

/// Write out every transitional verb the plain sink is still holding. The
/// normal path drains at `end`; a command that fails before reaching it
/// calls this from the error exit, so a log still records what was in
/// flight when it broke.
pub fn flush() {
	buffer_drain();
}

#[cfg(test)]
pub(crate) fn is_noop_final_for_tests(verb: &str) -> bool {
	is_noop_final(verb)
}

#[cfg(test)]
pub(crate) fn buffered_count_for_tests() -> usize {
	PLAIN_BUFFER.lock().map(|b| b.len()).unwrap_or(0)
}

/// Report that work on a resource has finished. Always returns `true` once
/// the event has been emitted (or buffered); the caller must not print its own
/// line in that case. Returns `false` only for an unrecognised `kind`, which
/// the caller treats as "no event happened" (#1673 ,  the plain-sink buffer
/// logic used to live partly here and partly in `ui::progress_line`, and a
/// `Sink::None` finish call dropped the buffer entry on the floor).
pub(super) fn finish(kind: &str, name: &str, verb: &str) -> bool {
	let Some(kind) = Kind::from_noun(kind) else {
		return false;
	};
	// Short-circuit the common "no board open" path before taking the lock
	// (#1364). `progress_line` is the hottest UI site: a 100-service `up`
	// fires it 100 times, and every call would otherwise acquire and release
	// the global `SESSION` mutex just to discover there is no session. With
	// the flag, the lock is only taken when `begin` was actually called.
	let sink = if !SESSION_OPEN.load(std::sync::atomic::Ordering::Acquire) {
		Sink::None
	} else {
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
	// `Sink::None` means there is no board ,  still go through `emit` so the
	// plain-sink buffer logic runs in the no-board path too. `emit` decides
	// what to actually emit: a final verb goes to stderr immediately, a
	// transitional verb goes to the buffer to be flushed at `end`.
	emit(sink, kind, name, verb);
	true
}

/// Close the board: stop the ticker, restore the cursor.
///
/// Idempotent, because the commands that open a board have several exits.
pub fn end() {
	#[cfg(test)]
	capture::record_end();
	if !SESSION_OPEN.swap(false, std::sync::atomic::Ordering::AcqRel) {
		// No board was ever opened ,  the flag is the single source of truth
		// for that. Drop it first so a racing `finish` doesn't take the lock
		// just to find an empty session (#1364).
		buffer_drain();
		return;
	}
	if let Ok(mut slot) = TICKER.lock() {
		if let Some(handle) = slot.take() {
			handle.abort();
		}
	}
	let Ok(mut slot) = SESSION.lock() else {
		buffer_drain();
		return;
	};
	if let Some(session) = slot.take() {
		// Walk the cursor back over the live region so the trailing rows are
		// not left dangling, then erase from there. The rows that were in
		// the live region stay where they are on screen ,  they are the
		// permanent record at this point, no different from scrollback. A
		// re-paint here would write the same bytes again, and a `script`
		// capture would show the row twice (once written, once erased, once
		// written again); keeping the cursor move without a re-write makes
		// the row appear exactly once in the capture (#1675).
		if let Some(region) = session.region.as_ref() {
			region.close_out();
		}
	}
	// Flush any buffered transitional verbs that never received a final one.
	// The plain sink is the only one that buffers, but `end` is the right place
	// to drain whatever the board left behind (#1673).
	buffer_drain();
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
	// every later line wrapping ,  and wrapping is what breaks the arithmetic.
	// Only the width is re-read; the terminal+colour decision is cached at
	// `begin` (#1364).
	let width = live_width().unwrap_or(0);
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
		lines.push(row::summary(done, total, width));
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
#[path = "mod_tests.rs"]
mod tests;
