//! Test-only record of the boards a command opens.
//!
//! `begin` and `end` are the whole of "this command draws the board", and
//! neither leaves anything behind that a test could read afterwards: `end`
//! takes the session. Recording the two calls is what lets a command be
//! checked against a fake Podman with no terminal in sight, which is the only
//! place the nine lifecycle commands can be driven in a unit test.
//!
//! Per thread, not process-global, and independent of whether progress output
//! is on. Both follow from the suite: 1800-odd tests run in parallel over one
//! process, so a shared record would mix one test's `begin` with another's, and
//! switching the process-wide progress flag on to observe a board would make
//! every *other* test's lifecycle call start recording one.

use std::cell::{Cell, RefCell};

use super::Kind;

/// One recorded call.
#[derive(Debug, Clone)]
pub(crate) enum Event {
	/// A [`super::begin`], with the rows it seeded and whether a live region
	/// would be opened for them. `live` is false under `cargo test`, where
	/// stderr is redirected, which is the same condition a pipe puts podup in.
	Begin {
		rows: Vec<(Kind, String)>,
		live: bool,
	},
	/// An [`super::start`]. The verb is the same string the live row would
	/// repaint with, so a test can assert what the row actually said.
	Start {
		kind: Kind,
		name: String,
		verb: String,
	},
	/// A [`super::finish`], the closing verb that turns `Pulling` into
	/// `Pulled`. Recorded alongside `start` so a test can assert the full
	/// row lifecycle in one place.
	Finish {
		kind: Kind,
		name: String,
		verb: String,
	},
	/// An [`super::end`].
	End,
}

thread_local! {
	/// Whether this thread is inside a [`Capture`]. Without it every test in
	/// the suite would accumulate events nobody reads.
	static RECORDING: Cell<bool> = const { Cell::new(false) };
	static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn record_begin(rows: &[(Kind, String)], live: bool) {
	if !RECORDING.get() {
		return;
	}
	EVENTS.with_borrow_mut(|events| {
		events.push(Event::Begin {
			rows: rows.to_vec(),
			live,
		})
	});
}

/// Record a `start` call. The verb is what the live row would repaint with,
/// captured so a test can assert what the row actually said. `None` for the
/// `Kind` means `from_noun` failed upstream, which `start` already short-circuits
/// on, so callers only invoke this when `kind` is `Some`.
pub(super) fn record_start(kind: Kind, name: &str, verb: &str) {
	if !RECORDING.get() {
		return;
	}
	EVENTS.with_borrow_mut(|events| {
		events.push(Event::Start {
			kind,
			name: name.to_string(),
			verb: verb.to_string(),
		})
	});
}

/// Record a `finish` call, paired with `record_start`. Captures the closing
/// verb the row was closed with so a test can assert the row actually reached
/// `Pulled` (or `Failed`), not just that something fired. `pub(crate)` so the
/// `progress_line` short-circuit can still record on the way out, without
/// having to take the SESSION lock on every call (#1364).
pub(crate) fn record_finish(kind: Kind, name: &str, verb: &str) {
	if !RECORDING.get() {
		return;
	}
	EVENTS.with_borrow_mut(|events| {
		events.push(Event::Finish {
			kind,
			name: name.to_string(),
			verb: verb.to_string(),
		})
	});
}

pub(super) fn record_end() {
	if !RECORDING.get() {
		return;
	}
	EVENTS.with_borrow_mut(|events| events.push(Event::End));
}

/// A recording session, cleared when it drops, including the drop that runs
/// while a failing test is unwinding.
pub(crate) struct Capture {
	/// Not constructible from outside, so the recording flag cannot be left on
	/// by a caller that builds one itself.
	_private: (),
}

impl Capture {
	/// Begin recording the calls made on this thread.
	pub(crate) fn start() -> Self {
		EVENTS.with_borrow_mut(|events| events.clear());
		RECORDING.set(true);
		Self { _private: () }
	}

	/// The rows of every board opened so far, in order.
	pub(crate) fn boards(&self) -> Vec<Vec<(Kind, String)>> {
		EVENTS.with_borrow(|events| {
			events
				.iter()
				.filter_map(|e| match e {
					Event::Begin { rows, .. } => Some(rows.clone()),
					Event::Start { .. } => None,
					Event::Finish { .. } => None,
					Event::End => None,
				})
				.collect()
		})
	}

	/// The rows of the one board a test that opens exactly one is about.
	pub(crate) fn rows(&self) -> Vec<(Kind, String)> {
		let boards = self.boards();
		assert_eq!(boards.len(), 1, "expected exactly one board: {boards:?}");
		boards.into_iter().next().unwrap_or_default()
	}

	/// The names on that one board, which is what most assertions are about.
	pub(crate) fn names(&self) -> Vec<String> {
		self.rows().into_iter().map(|(_, name)| name).collect()
	}

	/// Every row verb the engine asked the board to render, in order, joined
	/// across `start` and `finish` calls. Filter by `(Kind, name)` upstream to
	/// isolate a single resource, since multiple boards may overlap in the
	/// thread-local log.
	///
	/// `start` rows are working verbs (`Pulling`, `Pulling 2/4`); `finish` rows
	/// are closing verbs (`Pulled`, `Failed`). A test that wants the row
	/// lifecycle end-to-end filters both, then walks the list, which is what
	/// `a_piped_pull_prints_only_pulling_and_pulled` does to assert that no
	/// intermediate verbs slipped through.
	pub(crate) fn verbs(&self) -> Vec<(Kind, String, String)> {
		EVENTS.with_borrow(|events| {
			events
				.iter()
				.filter_map(|e| match e {
					Event::Start { kind, name, verb } => Some((*kind, name.clone(), verb.clone())),
					Event::Finish { kind, name, verb } => Some((*kind, name.clone(), verb.clone())),
					_ => None,
				})
				.collect()
		})
	}

	/// Whether every board that was opened was also closed. An unclosed board
	/// on a terminal leaves the cursor hidden, so this is not a detail.
	pub(crate) fn every_board_ended(&self) -> bool {
		EVENTS.with_borrow(|events| {
			let begins = events
				.iter()
				.filter(|e| matches!(e, Event::Begin { .. }))
				.count();
			let ends = events.iter().filter(|e| matches!(e, Event::End)).count();
			begins > 0 && begins == ends
		})
	}

	/// Whether any board would have opened a live region. False with stderr
	/// redirected, which is how a test reads the plain-line half of the
	/// contract.
	pub(crate) fn any_live(&self) -> bool {
		EVENTS.with_borrow(|events| {
			events.iter().any(|e| match e {
				Event::Begin { live, .. } => *live,
				Event::Start { .. } => false,
				Event::Finish { .. } => false,
				Event::End => false,
			})
		})
	}
}

impl Drop for Capture {
	fn drop(&mut self) {
		RECORDING.set(false);
		EVENTS.with_borrow_mut(|events| events.clear());
	}
}
