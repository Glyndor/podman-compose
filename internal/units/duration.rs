//! Durations rendered as composite components or as plain milliseconds.

use std::time::Duration;

/// The duration ladder, largest first, in nanoseconds.
///
/// A year is exactly 365 days and a month exactly 30. A duration is an elapsed
/// span, not a range between two dates, so there is no calendar to consult for a
/// leap day or for which month it fell in; both numbers are ones a reader can
/// redo in their head.
///
/// The two do not divide evenly, and that is visible: 364 days reads `12mo 4d`
/// while 365 reads `1y`. Twelve thirty-day months are 360 days, not a year. The
/// alternative is a fractional month (365/12 = 30.4167 days), which no reader
/// can check by hand: the seam is preferred over the arithmetic nobody can
/// verify.
///
/// No week. Unlike a month it has no conventional length in a duration context
/// at all: it only means something against a calendar, and `9d` already says
/// what `1w 2d` would.
const UNITS: [(&str, u128); 9] = [
	("y", 365 * 24 * 60 * 60 * 1_000_000_000),
	("mo", 30 * 24 * 60 * 60 * 1_000_000_000),
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
	/// Up to N components, largest first, zeros skipped: `1h 5m 3s`,
	/// `1y 2mo 3d`, `340ms`.
	///
	/// The component count is fixed; which units fill it is not. A span that
	/// just started shows `5s`, one running an hour shows `1h 5m 3s`, and one
	/// past a year shows `1y 2mo 3d`: the window slides up the ladder as the
	/// span grows, so the same width always carries the three units that matter
	/// at that magnitude.
	Parts(usize),
	/// One number in milliseconds with `decimals` decimal places: `340.00ms`.
	/// For machine reading, or when the reader wants to compare two durations
	/// rather than skim one.
	///
	/// No caller yet: the benchmark aggregation is where it lands, and that is
	/// Python today (`bench/aggregate.py`). Exercised by this module's tests
	/// meanwhile; the allow is on this variant alone so anything else going dead
	/// here still warns.
	#[allow(dead_code)]
	Millis { decimals: usize },
}

impl DurationFormat {
	/// Three components, which is the width the owner asked for: enough to say
	/// `1h 5m 3s` or `1y 2mo 3d` without running on into `200ms`.
	///
	/// The count is shared with [`SizeFormat::default_parts`] rather than
	/// written twice, so sizes and durations cannot drift to different defaults.
	///
	/// [`SizeFormat::default_parts`]: super::SizeFormat::default_parts
	pub(crate) const fn default_parts() -> Self {
		Self::Parts(super::bytes::DEFAULT_PARTS)
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
