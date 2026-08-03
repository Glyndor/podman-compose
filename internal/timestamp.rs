//! RFC 3339 timestamps, as libpod puts them on the wire.
//!
//! podup has no date library and does not want one. The three fields that need
//! this — a container's `Created`, a volume's `CreatedAt`, and an image's
//! `Created` from the inspect endpoint — all arrive as RFC 3339 strings, and
//! everything else libpod reports (`StartedAt`, the event feed) is already Unix
//! seconds.
//!
//! Two flavours appear in practice, measured on Podman 5.7.0: the image inspect
//! response ends in `Z`, and the container list carries an explicit offset
//! (`-05:00`). A parser that handles one and not the other fails on exactly half
//! the columns, so both are covered here and by tests built from captured
//! strings rather than from the specification.
//!
//! Parsing is fail-closed: anything that is not a well-formed timestamp in range
//! returns `None`, and callers render an empty cell. A cell that is blank tells
//! the reader podup could not say; a cell holding a plausible wrong date does
//! not.

/// Seconds in a day.
const SECS_PER_DAY: i64 = 86_400;

/// Parse an RFC 3339 timestamp into Unix seconds.
///
/// Accepts `YYYY-MM-DDTHH:MM:SS` followed by an optional fractional part and a
/// mandatory offset of `Z` or `±HH:MM`. The date/time separator may be `T`, `t`
/// or a space, which RFC 3339 permits and which keeps this usable for the
/// `--since` values a user types by hand.
///
/// Returns `None` for anything malformed or out of range, including a date that
/// does not exist (31 February) and an offset beyond ±24 hours. The fractional
/// part is validated but discarded: every consumer here renders whole seconds at
/// coarsest, and keeping sub-second precision would only widen the type.
pub(crate) fn parse_rfc3339(input: &str) -> Option<i64> {
	let bytes = input.as_bytes();
	// `YYYY-MM-DDTHH:MM:SS` is nineteen characters, and an offset always
	// follows, so anything shorter than twenty cannot be complete.
	if bytes.len() < 20 {
		return None;
	}

	let year: i64 = digits(&input[0..4])?;
	if bytes[4] != b'-' {
		return None;
	}
	let month: i64 = digits(&input[5..7])?;
	if bytes[7] != b'-' {
		return None;
	}
	let day: i64 = digits(&input[8..10])?;
	if !matches!(bytes[10], b'T' | b't' | b' ') {
		return None;
	}
	let hour: i64 = digits(&input[11..13])?;
	if bytes[13] != b':' {
		return None;
	}
	let minute: i64 = digits(&input[14..16])?;
	if bytes[16] != b':' {
		return None;
	}
	let second: i64 = digits(&input[17..19])?;

	// A leap second is reported as :60 and is not a distinct instant here, so it
	// is folded onto the following second rather than rejected: refusing a
	// timestamp the server legitimately sent would blank the cell.
	if !(1..=12).contains(&month) || day < 1 || hour > 23 || minute > 59 || second > 60 {
		return None;
	}
	if day > days_in_month(year, month) {
		return None;
	}

	let offset = parse_offset(&input[19..])?;

	let days = days_from_civil(year, month, day);
	let secs = days * SECS_PER_DAY + hour * 3600 + minute * 60 + second;
	Some(secs - offset)
}

/// Parse the fractional part and the zone suffix into an offset in seconds.
///
/// The offset is what has to be *subtracted* to reach UTC: a timestamp written
/// at `-05:00` is five hours behind, so its UTC instant is five hours later.
fn parse_offset(rest: &str) -> Option<i64> {
	let bytes = rest.as_bytes();
	let mut i = 0;

	// Optional fractional seconds. Validated so `.` alone or `.abc` is rejected
	// rather than silently treated as a zone that is not there.
	if bytes.first() == Some(&b'.') {
		i = 1;
		let start = i;
		while i < bytes.len() && bytes[i].is_ascii_digit() {
			i += 1;
		}
		if i == start {
			return None;
		}
	}

	match bytes.get(i) {
		Some(b'Z' | b'z') if i + 1 == bytes.len() => Some(0),
		Some(sign @ (b'+' | b'-')) => {
			// `±HH:MM`, exactly six characters, nothing after it.
			let zone = rest.get(i + 1..)?;
			if zone.len() != 5 || zone.as_bytes()[2] != b':' {
				return None;
			}
			let hours: i64 = digits(&zone[0..2])?;
			let minutes: i64 = digits(&zone[3..5])?;
			if hours > 23 || minutes > 59 {
				return None;
			}
			let magnitude = hours * 3600 + minutes * 60;
			Some(if *sign == b'-' { -magnitude } else { magnitude })
		}
		_ => None,
	}
}

/// Parse a run of ASCII digits, rejecting a sign, spaces or anything else
/// `str::parse` would otherwise accept.
///
/// `"+1"` and `" 1"` both parse as one through the standard parser, which would
/// let `20 6-08-02` through the field checks above.
fn digits(s: &str) -> Option<i64> {
	if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
		return None;
	}
	s.parse().ok()
}

/// Whether `year` is a leap year in the proleptic Gregorian calendar.
const fn is_leap_year(year: i64) -> bool {
	year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Days in `month` of `year`.
const fn days_in_month(year: i64, month: i64) -> i64 {
	match month {
		1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
		4 | 6 | 9 | 11 => 30,
		2 if is_leap_year(year) => 29,
		2 => 28,
		_ => 0,
	}
}

/// Days since the Unix epoch for a civil date.
///
/// The exact inverse of the civil-from-days walk in `engine::events`, which
/// turns Unix seconds back into a date. Both shift the epoch to 0000-03-01 so
/// February — the month whose length varies — lands last and the leap-day case
/// needs no branch of its own. Round-tripping one through the other is the test
/// that matters here: for both to agree and both be wrong, they would have to be
/// wrong in the same direction.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
	let year = year - i64::from(month <= 2);
	let era = if year >= 0 { year } else { year - 399 } / 400;
	let year_of_era = year - era * 400;
	let month_shifted = if month > 2 { month - 3 } else { month + 9 };
	let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
	let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
	era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
#[path = "timestamp_tests.rs"]
mod tests;
