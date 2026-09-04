//! The resource set a lifecycle command is working through, and where each one
//! has got to.
//!
//! Pure: no terminal, no clock reading of its own, no I/O. A renderer asks it
//! what to draw and it answers; that is what makes the state machine testable
//! without a tty, which matters because the two things most likely to be wrong
//! here — a row that never leaves `Working`, and a count that disagrees with the
//! rows — are invisible in a screenshot.

use std::time::{Duration, Instant};

/// What kind of thing a row is about. The noun printed in the first column, and
/// the same vocabulary `progress_line` has always used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
	/// A compose network, project-scoped unless declared `external`.
	Network,
	/// A compose volume, project-scoped unless declared `external`.
	Volume,
	/// A Podman-native secret, which is what every compose `secrets:` and
	/// `configs:` entry becomes.
	Secret,
	/// A container image, whether pulled or built.
	Image,
	/// A container, one per replica rather than one per service.
	Container,
}

impl Kind {
	/// The displayed noun.
	pub fn noun(self) -> &'static str {
		match self {
			Kind::Network => "Network",
			Kind::Volume => "Volume",
			Kind::Secret => "Secret",
			Kind::Image => "Image",
			Kind::Container => "Container",
		}
	}

	/// Parse the noun back, so `progress_line`'s existing `&str` callers can feed
	/// the board without all 21 of them changing shape.
	pub fn from_noun(noun: &str) -> Option<Self> {
		match noun {
			"Network" => Some(Kind::Network),
			"Volume" => Some(Kind::Volume),
			"Secret" => Some(Kind::Secret),
			"Image" => Some(Kind::Image),
			"Container" => Some(Kind::Container),
			_ => None,
		}
	}
}

/// Where a resource has got to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
	/// Seeded, nothing has happened to it yet.
	Pending,
	/// Work is under way; the verb is the present participle (`Creating`).
	Working(String),
	/// Work finished; the verb is the past tense (`Created`).
	Done(String),
}

/// One resource's row.
#[derive(Debug, Clone)]
pub struct Row {
	/// Which noun this row is about; also the first column.
	pub kind: Kind,
	/// The resource's name as Podman knows it, project prefix included, so it
	/// matches what `podman ps` or `podman network ls` would show.
	pub name: String,
	/// Where the row is in its lifecycle. Ordering is `Pending`, `Working`,
	/// `Done`, and a row never goes backwards.
	pub state: State,
	/// When this row last entered `Working`, for the elapsed column. `None`
	/// while `Pending`, since nothing has taken any time yet.
	pub started: Option<Instant>,
	/// How long the row spent working, frozen when it reached `Done` so a
	/// finished row stops counting up.
	pub elapsed: Option<Duration>,
}

impl Row {
	/// How long to show against this row, or `None` when there is nothing to
	/// show yet.
	pub fn duration(&self, now: Instant) -> Option<Duration> {
		match (&self.state, self.elapsed, self.started) {
			(State::Done(_), Some(d), _) => Some(d),
			(State::Working(_), _, Some(start)) => Some(now.saturating_duration_since(start)),
			_ => None,
		}
	}
}

/// Every resource one command is working through, in the order it was seeded.
///
/// Seeded up front rather than grown as events arrive: a board whose rows is
/// grown from those events is a transcript with extra steps, and the whole
/// point is to show what is still to come.
#[derive(Debug, Default)]
pub struct Board {
	rows: Vec<Row>,
	/// Rows already handed to the renderer as permanent history, so they are
	/// drawn once and never repainted. Counted from the front: rows leave the
	/// live region in order.
	flushed: usize,
}

/// The position a row was inserted at.
///
/// Returned by [`Board::start`] and the related variants so the engine can
/// tell, when it needs to, whether it just created the row or found it
/// already seeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertedAt {
	/// The row already existed; the index it sits at in `rows`.
	Existing(usize),
	/// The row was just appended; its new index.
	Appended(usize),
}

impl Board {
	/// A board seeded with `resources` in the order they will be worked through.
	pub fn new(resources: impl IntoIterator<Item = (Kind, String)>) -> Self {
		Self {
			rows: resources
				.into_iter()
				.map(|(kind, name)| Row {
					kind,
					name,
					state: State::Pending,
					started: None,
					elapsed: None,
				})
				.collect(),
			flushed: 0,
		}
	}

	/// Mark a resource as being worked on. Unknown resources are appended rather
	/// than dropped: the seed is the best guess available before the work starts,
	/// and a compose file can grow a container the seed did not predict (an
	/// implicit `_default` network, a `--scale` override). Losing the row would
	/// be worse than a board that grows by one.
	pub fn start(&mut self, kind: Kind, name: &str, verb: &str, now: Instant) -> InsertedAt {
		if let Some(idx) = self.index_of(kind, name) {
			let row = &mut self.rows[idx];
			row.state = State::Working(verb.to_string());
			row.started.get_or_insert(now);
			return InsertedAt::Existing(idx);
		}
		self.rows.push(Row {
			kind,
			name: name.to_string(),
			state: State::Working(verb.to_string()),
			started: Some(now),
			elapsed: None,
		});
		InsertedAt::Appended(self.rows.len() - 1)
	}

	/// Mark a resource as being worked on, **inserting** a new row in front
	/// of `anchor` rather than appending it. Used by the `up` build path: the
	/// container rows are already seeded in front of the image row, so a
	/// plain `start` would put the image row *after* them, and the reader
	/// would see `Container … Starting` before the `Building` row that says
	/// which image is being built. A row that is already at or before the
	/// anchor is left alone, so this is also safe to call more than once.
	pub fn start_before(
		&mut self,
		anchor_kind: Kind,
		anchor_name: &str,
		kind: Kind,
		name: &str,
		verb: &str,
		now: Instant,
	) -> InsertedAt {
		let anchor_idx = self
			.index_of(anchor_kind, anchor_name)
			.unwrap_or(self.rows.len());
		if let Some(idx) = self.index_of(kind, name) {
			// Already on the board. If it sits at or before the anchor, the
			// order is already right and we only need to flip its state.
			if idx <= anchor_idx {
				let row = &mut self.rows[idx];
				row.state = State::Working(verb.to_string());
				row.started.get_or_insert(now);
				return InsertedAt::Existing(idx);
			}
			// Behind the anchor: lift the row to just before the anchor so
			// the image reads as the prerequisite of the container it lives
			// for.
			let row = self.rows.remove(idx);
			let insert_at = anchor_idx.min(self.rows.len());
			self.rows.insert(insert_at, row);
			let row = &mut self.rows[insert_at];
			row.state = State::Working(verb.to_string());
			row.started.get_or_insert(now);
			return InsertedAt::Existing(insert_at);
		}
		// Not on the board yet. Insert a fresh row just before the anchor.
		let insert_at = anchor_idx.min(self.rows.len());
		self.rows.insert(
			insert_at,
			Row {
				kind,
				name: name.to_string(),
				state: State::Working(verb.to_string()),
				started: Some(now),
				elapsed: None,
			},
		);
		InsertedAt::Appended(insert_at)
	}

	/// Mark a resource as finished. Also tolerates a resource that was never
	/// seeded or started — most of the 21 existing call sites report only an
	/// ending, so this has to work without a matching `start`.
	pub fn finish(&mut self, kind: Kind, name: &str, verb: &str, now: Instant) {
		let idx = self.index_of(kind, name).unwrap_or_else(|| {
			self.rows.push(Row {
				kind,
				name: name.to_string(),
				state: State::Pending,
				started: None,
				elapsed: None,
			});
			self.rows.len() - 1
		});
		let row = &mut self.rows[idx];
		row.elapsed = row.started.map(|s| now.saturating_duration_since(s));
		row.state = State::Done(verb.to_string());
	}

	/// Rows that are finished *and* sit at the front of the un-flushed range, so
	/// they can be printed once as permanent history and dropped from the region
	/// that gets repainted.
	///
	/// Only a contiguous run from the front is eligible. A finished row with an
	/// unfinished one before it has to stay in the live region, or the permanent
	/// history would print out of order — which is exactly the record `up` exists
	/// to leave behind.
	pub fn take_completed_prefix(&mut self) -> Vec<Row> {
		let mut out = Vec::new();
		while let Some(row) = self.rows.get(self.flushed) {
			if !matches!(row.state, State::Done(_)) {
				break;
			}
			out.push(row.clone());
			self.flushed += 1;
		}
		out
	}

	/// Every row the board holds, regardless of state, for the final paint at
	/// `progress::end`.
	///
	/// Used only at the close of a board: a row that was still in flight during
	/// the last `repaint` (and so already painted in the live region) is
	/// re-rendered here as part of the permanent record. Previously `end`
	/// re-painted the live region too, which made a non-contiguous `Done` row
	/// like `Failed` appear twice in the final output (#1675). Here the live
	/// region is collapsed: every row is scrollback exactly once.
	pub fn take_all_rows(&mut self) -> Vec<Row> {
		let out = self.rows.clone();
		self.flushed = self.rows.len();
		out
	}

	/// The rows still in the live region: everything not yet flushed.
	pub fn live_rows(&self) -> &[Row] {
		&self.rows[self.flushed.min(self.rows.len())..]
	}

	/// How many resources have finished, and how many there are in total — the
	/// `3/6` in the summary line.
	pub fn tally(&self) -> (usize, usize) {
		(
			self.rows
				.iter()
				.filter(|r| matches!(r.state, State::Done(_)))
				.count(),
			self.rows.len(),
		)
	}

	/// Whether every seeded resource has finished.
	pub fn is_complete(&self) -> bool {
		let (done, total) = self.tally();
		done == total
	}

	fn index_of(&self, kind: Kind, name: &str) -> Option<usize> {
		self.rows
			.iter()
			.position(|r| r.kind == kind && r.name == name)
	}
}

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;
