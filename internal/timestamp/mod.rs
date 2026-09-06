//! RFC 3339 timestamps, as libpod puts them on the wire.
//!
//! podup has no date library and does not want one. The three fields that need
//! this (a container's `Created`, a volume's `CreatedAt`, and an image's
//! `Created` from the inspect endpoint) all arrive as RFC 3339 strings, and
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
/// The exact inverse of [`civil_from_days`]. Both shift the epoch to 0000-03-01
/// so February, the month whose length varies, lands last and the leap-day
/// case needs no branch of its own. Round-tripping one through the other is the
/// test that matters here: for both to agree and both be wrong, they would have
/// to be wrong in the same direction.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
	let year = year - i64::from(month <= 2);
	let era = if year >= 0 { year } else { year - 399 } / 400;
	let year_of_era = year - era * 400;
	let month_shifted = if month > 2 { month - 3 } else { month + 9 };
	let day_of_year = (153 * month_shifted + 2) / 5 + day - 1;
	let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
	era * 146_097 + day_of_era - 719_468
}

/// The civil date for a count of days since the Unix epoch, as
/// `(year, month, day)`.
///
/// The exact inverse of [`days_from_civil`]. This used to live in
/// `engine::events` as half of its timestamp formatter; the two halves are one
/// thing and belong next to each other, and the round-trip test between them is
/// only honest while neither can be edited without the other in view.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
	let shifted = days + 719_468;
	let era = shifted.div_euclid(146_097);
	let day_of_era = shifted.rem_euclid(146_097);
	let year_of_era =
		(day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
	let year = year_of_era + era * 400;
	let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
	let month_shifted = (5 * day_of_year + 2) / 153;
	let day = day_of_year - (153 * month_shifted + 2) / 5 + 1;
	let month = if month_shifted < 10 {
		month_shifted + 3
	} else {
		month_shifted - 9
	};
	let year = if month <= 2 { year + 1 } else { year };
	(year, month, day)
}

/// Render `unix_secs` as a wall-clock timestamp in the reader's own time zone,
/// with the offset that applies **at that instant**: `2026-08-02 23:43:35 -05:00`.
///
/// Matching what `podman events` prints. Before this, podup rendered UTC and did
/// not say so, which is worse than either alternative: a reader correlating a
/// podup line against `podman events` or `journalctl` reads it as their own wall
/// clock and is silently wrong by the offset.
///
/// The offset is resolved per instant rather than once, so an event from July
/// and one from January render correctly wherever daylight saving applies. When
/// the platform cannot say what the offset is, the value is rendered in UTC and
/// labelled `Z`; an unlabelled guess is the failure this replaced.
pub(crate) fn format_local(unix_secs: i64) -> String {
	render_with_offset(unix_secs, local_offset_seconds(unix_secs))
}

/// The rendering half, split from the lookup so both of its branches can be
/// entered from a test.
///
/// Two things follow from the split, and both are why it exists. The `None`
/// branch is unreachable through [`format_local`] on any machine whose libc
/// resolves a zone: mutations deleting it survived every test that went in the
/// front door, which is what an untestable control looks like from outside. And
/// the rendered string stops depending on the machine's own zone, so it can be
/// pinned exactly instead of only by shape.
fn render_with_offset(unix_secs: i64, offset: Option<i64>) -> String {
	let Some(offset) = offset else {
		return format!("{}Z", format_civil(unix_secs));
	};
	let sign = if offset < 0 { '-' } else { '+' };
	let magnitude = offset.abs();
	format!(
		"{} {sign}{:02}:{:02}",
		format_civil(unix_secs + offset),
		magnitude / 3600,
		(magnitude % 3600) / 60
	)
}

/// `YYYY-MM-DD HH:MM:SS` for a count of seconds, with no zone of its own.
///
/// `div_euclid`/`rem_euclid` rather than `/` and `%` so a negative input floors
/// instead of truncating toward zero; with the plain operators a pre-epoch
/// instant renders an hour of -1.
fn format_civil(secs_total: i64) -> String {
	let days = secs_total.div_euclid(SECS_PER_DAY);
	let secs = secs_total.rem_euclid(SECS_PER_DAY);
	let (year, month, day) = civil_from_days(days);
	format!(
		"{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
		secs / 3600,
		(secs % 3600) / 60,
		secs % 60
	)
}

#[cfg(unix)]
#[path = "offset_unix.rs"]
mod offset;
#[cfg(windows)]
#[path = "offset_windows.rs"]
mod offset;

/// Offset from UTC in seconds at `unix_secs`, or `None` when the platform
/// cannot say.
///
/// Split per platform because the two ask entirely different questions of the
/// OS, and both need FFI: there is no portable way to reach a timezone database
/// from the standard library.
fn local_offset_seconds(unix_secs: i64) -> Option<i64> {
	offset::local_offset_seconds(unix_secs)
}

/// Build a Win32 `SYSTEMTIME` from Unix seconds.
///
/// Lives here rather than in the Windows module so it is compiled, and
/// therefore type-checked and unit-tested, on every platform. A conversion
/// that only builds on the one machine nobody develops on is a conversion
/// nobody has checked.
#[cfg(windows)]
fn to_system_time(unix_secs: i64) -> Option<windows_sys::Win32::Foundation::SYSTEMTIME> {
	let days = unix_secs.div_euclid(SECS_PER_DAY);
	let secs = unix_secs.rem_euclid(SECS_PER_DAY);
	let (year, month, day) = civil_from_days(days);
	Some(windows_sys::Win32::Foundation::SYSTEMTIME {
		wYear: u16::try_from(year).ok()?,
		wMonth: u16::try_from(month).ok()?,
		wDayOfWeek: 0,
		wDay: u16::try_from(day).ok()?,
		wHour: u16::try_from(secs / 3600).ok()?,
		wMinute: u16::try_from((secs % 3600) / 60).ok()?,
		wSecond: u16::try_from(secs % 60).ok()?,
		wMilliseconds: 0,
	})
}

/// Unix seconds from a Win32 `SYSTEMTIME`, ignoring milliseconds.
#[cfg(windows)]
fn from_system_time(t: &windows_sys::Win32::Foundation::SYSTEMTIME) -> i64 {
	let days = days_from_civil(i64::from(t.wYear), i64::from(t.wMonth), i64::from(t.wDay));
	days * SECS_PER_DAY
		+ i64::from(t.wHour) * 3600
		+ i64::from(t.wMinute) * 60
		+ i64::from(t.wSecond)
}

#[cfg(test)]
mod tests;
