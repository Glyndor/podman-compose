//! Durations rendered as composite components or as plain milliseconds.

use std::time::Duration;

/// The duration ladder, largest first, in nanoseconds.
///
/// No month and no week, deliberately. A month is 28 to 31 days and a week only
/// means something against a calendar, while a duration has neither — and an
/// uptime of `1mo` tells the reader less than `34d` does.
///
/// A year here is exactly 365 days. A duration is an elapsed span, not a range
/// between two dates, so there is no calendar to consult for a leap day; 365 is
/// the arithmetic a reader can redo in their head.
const UNITS: [(&str, u128); 8] = [
	("y", 365 * 24 * 60 * 60 * 1_000_000_000),
	("d", 24 * 60 * 60 * 1_000_000_000),
	("h", 60 * 60 * 1_000_000_000),
	("m", 60 * 1_000_000_000),
	("s", 1_000_000_000),
	("ms", 1_000_000),
	("µs", 1_000),
	("ns", 1),
];

/// Nanoseconds in a millisecond, for the plain-milliseconds shape.
const NANOS_PER_MILLI: f64 = 1_000_000.0;

/// How to render a duration.
///
/// The two shapes take different settings and neither means anything under the
/// other, so they are separate variants rather than fields a caller could set
/// in a combination that has no rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DurationFormat {
	/// Up to N components, largest first, zeros skipped: `2h 5m`,
	/// `1y 1d 1h 4s 5ms`, `340ms`. What reads at a glance.
	Parts(usize),
	/// One number in milliseconds with `decimals` decimal places: `340.00ms`.
	/// For machine reading, or when the reader wants to compare two durations
	/// rather than skim one.
	Millis { decimals: usize },
}

impl DurationFormat {
	/// Two components, which is what reads at a glance: `2h 5m`, not
	/// `2h 5m 3s 200ms`.
	pub(crate) const fn default_parts() -> Self {
		Self::Parts(2)
	}
}

/// Render `duration` as a human-readable span.
///
/// Total over `Duration`: the ladder is walked in `u128` nanoseconds, which
/// `Duration::MAX` cannot exceed, so nothing saturates. A zero duration renders
/// `0s` under either shape rather than an empty cell.
pub(crate) fn format_duration(duration: Duration, fmt: &DurationFormat) -> String {
	match *fmt {
		DurationFormat::Parts(parts) => composite(duration.as_nanos(), parts),
		DurationFormat::Millis { decimals } => {
			let millis = duration.as_nanos() as f64 / NANOS_PER_MILLI;
			format!("{millis:.decimals$}ms")
		}
	}
}

/// Up to `parts` whole components, largest first, zeros skipped.
///
/// Skipping zeros rather than stopping at the first one is what makes
/// `1y 1d 1h 4s 5ms` possible: that span has no months by design and no minutes
/// by accident, and both are absent from the output for the same reason.
fn composite(nanos: u128, parts: usize) -> String {
	let wanted = parts.max(1);
	let mut remainder = nanos;
	let mut out: Vec<String> = Vec::with_capacity(wanted);

	for (unit, scale) in UNITS {
		if out.len() == wanted {
			break;
		}
		let count = remainder / scale;
		if count > 0 {
			out.push(format!("{count}{unit}"));
			remainder %= scale;
		}
	}

	if out.is_empty() {
		return "0s".to_string();
	}
	out.join(" ")
}

#[cfg(test)]
#[path = "duration_tests.rs"]
mod tests;
