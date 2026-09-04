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
