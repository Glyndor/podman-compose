use super::parse_rfc3339;

/// The two strings libpod actually sent, captured from Podman 5.7.0 on
/// 2026-08-03 rather than written from the specification. One ends in `Z` (the
/// image inspect response), the other carries an explicit offset (the container
/// list). A parser that handles one and not the other blanks half the columns.
#[test]
fn the_two_shapes_libpod_really_sends_both_parse() {
	// `docker.io/library/redis:8-alpine`, from `images/{ref}/json`.
	assert_eq!(
		parse_rfc3339("2026-05-07T17:41:42.866453985Z"),
		Some(1_778_175_702)
	);
	// A running container, from `containers/json`. Written at -05:00, so its
	// UTC instant is five hours later than the wall clock in the string:
	// 2026-08-03 00:58:45Z. Both constants here were checked against an
	// independent parser rather than worked out by hand — the first version of
	// this one was an hour off, and the code was right.
	assert_eq!(
		parse_rfc3339("2026-08-02T19:58:45.39802971-05:00"),
		Some(1_785_718_725)
	);
}

/// The offset is applied in the right direction, which is the single easiest
/// thing to get backwards. The same wall clock at three zones is three
/// different instants, five hours apart in each direction.
#[test]
fn the_offset_moves_the_instant_the_right_way() {
	let utc = parse_rfc3339("2026-01-01T12:00:00Z").unwrap();
	let behind = parse_rfc3339("2026-01-01T12:00:00-05:00").unwrap();
	let ahead = parse_rfc3339("2026-01-01T12:00:00+05:00").unwrap();
	assert_eq!(behind - utc, 5 * 3600, "a negative offset is behind UTC");
	assert_eq!(utc - ahead, 5 * 3600, "a positive offset is ahead of UTC");
	// And a half-hour zone, since minutes are a separate field.
	assert_eq!(
		parse_rfc3339("2026-01-01T12:00:00+05:30").unwrap(),
		utc - (5 * 3600 + 30 * 60)
	);
}

/// Round-trip through the formatter that lives beside this parser. The two are
/// inverses — days-from-civil here, civil-from-days there — so for both to agree
/// and both be wrong they would have to be wrong in the same direction.
///
/// **Zone-independent on purpose.** `format_local` renders the reader's wall
/// clock, so pinning its exact output would pass on a machine at -05:00 and fail
/// on a CI runner at UTC. Reading the rendered offset back and undoing it must
/// return the instant that went in, on any machine, which is the stronger claim
/// anyway: it checks the offset is applied in the right direction as well as
/// that the calendar arithmetic holds.
#[test]
fn format_local_round_trips_on_any_machine() {
	for unix in [
		0_i64,
		1,
		-1,
		-86_400,
		68_169_600,    // 1972-02-29, a leap day
		951_782_400,   // 2000-02-29, the century leap a naive rule misses
		1_709_164_800, // 2024-02-29
		1_735_689_599, // last second of 2024
		1_735_689_600, // first of 2025
		1_785_718_245, // the instant the TIME column was added for
		4_102_444_800, // 2100-01-01, a century that is not a leap year
	] {
		let rendered = super::format_local(unix);
		let reparsed = reparse(&rendered)
			.unwrap_or_else(|| panic!("could not reparse {rendered:?} (from {unix})"));
		assert_eq!(reparsed, unix, "{rendered:?} did not round-trip");
	}
}

/// Turn what `format_local` prints back into an RFC 3339 string this module can
/// parse: `YYYY-MM-DD HH:MM:SS ±HH:MM` differs from RFC 3339 only by the space
/// before the offset, and `Z` needs no change at all.
fn reparse(rendered: &str) -> Option<i64> {
	let normalised = match rendered.rsplit_once(' ') {
		Some((instant, zone)) if zone.starts_with(['+', '-']) => format!("{instant}{zone}"),
		_ => rendered.to_string(),
	};
	parse_rfc3339(&normalised)
}

/// The rendered shape itself, which the round-trip above would tolerate being
/// wrong about: twenty-six characters, and an offset that is `Z` or `±HH:MM`.
/// The `events` TIME column is sized from this, so a change here that nothing
/// notices truncates every row.
#[test]
fn format_local_renders_a_fixed_width_shape() {
	let rendered = super::format_local(1_785_718_245);
	assert_eq!(
		rendered.len(),
		26,
		"{rendered:?} is not the width the TIME column is sized for"
	);
	let (instant, zone) = rendered.rsplit_once(' ').expect("no zone in {rendered:?}");
	assert_eq!(instant.len(), 19, "{instant:?}");
	assert!(
		zone == "Z" || (zone.len() == 6 && zone.starts_with(['+', '-']) && &zone[3..4] == ":"),
		"{zone:?} is not Z or ±HH:MM"
	);
}

/// The rendering, pinned exactly. Possible because the offset is a parameter
/// here rather than a reading of the machine's own zone, so these strings are
/// the same on a laptop at -05:00 and a runner at UTC.
#[test]
fn the_rendering_is_pinned_for_every_offset_shape() {
	let t = 1_785_718_245; // 2026-08-03 00:50:45 UTC
	assert_eq!(
		super::render_with_offset(t, Some(-5 * 3600)),
		"2026-08-02 19:50:45 -05:00"
	);
	assert_eq!(
		super::render_with_offset(t, Some(5 * 3600 + 30 * 60)),
		"2026-08-03 06:20:45 +05:30"
	);
	assert_eq!(
		super::render_with_offset(t, Some(0)),
		"2026-08-03 00:50:45 +00:00"
	);
	// A negative offset with minutes, so the sign is not carried by the hours
	// alone: -09:30 is a real zone (Marquesas).
	assert_eq!(
		super::render_with_offset(t, Some(-(9 * 3600 + 30 * 60))),
		"2026-08-02 15:20:45 -09:30"
	);
}

/// When the platform cannot say what the offset is, the value renders in UTC
/// and says so. An unlabelled guess is the defect this whole change replaced,
/// so the fallback must not quietly become one.
///
/// Only reachable at this level: on any machine whose libc resolves a zone,
/// `format_local` never takes this branch.
#[test]
fn an_unknown_offset_renders_utc_and_labels_it() {
	assert_eq!(
		super::render_with_offset(1_785_718_245, None),
		"2026-08-03 00:50:45Z"
	);
	assert_eq!(super::render_with_offset(0, None), "1970-01-01 00:00:00Z");
}

/// The calendar half, pinned absolutely because it has no zone in it. This is
/// what the round-trip cannot check on its own: two inverse functions can agree
/// while both being shifted by the same amount.
#[test]
fn the_civil_rendering_is_pinned_to_known_instants() {
	assert_eq!(super::format_civil(0), "1970-01-01 00:00:00");
	assert_eq!(super::format_civil(1), "1970-01-01 00:00:01");
	// 1972 was a leap year: 29 February exists.
	assert_eq!(super::format_civil(68_169_600), "1972-02-29 00:00:00");
	assert_eq!(super::format_civil(68_256_000), "1972-03-01 00:00:00");
	// 1900 is divisible by 4 and NOT a leap year; 2000 is divisible by 400 and
	// is one. The second is the case a naive rule gets wrong.
	assert_eq!(super::format_civil(951_782_400), "2000-02-29 00:00:00");
	// Last second of a year, then the first of the next.
	assert_eq!(super::format_civil(1_735_689_599), "2024-12-31 23:59:59");
	assert_eq!(super::format_civil(1_735_689_600), "2025-01-01 00:00:00");
	// The event the column was added for, captured from libpod.
	assert_eq!(super::format_civil(1_785_718_245), "2026-08-03 00:50:45");
}

/// Before the epoch. `div_euclid`/`rem_euclid` are used rather than `/` and `%`
/// precisely so a negative input floors instead of truncating toward zero; with
/// the plain operators this renders an hour of -1.
#[test]
fn the_civil_rendering_handles_dates_before_the_epoch() {
	assert_eq!(super::format_civil(-1), "1969-12-31 23:59:59");
	assert_eq!(super::format_civil(-86_400), "1969-12-31 00:00:00");
}

/// Dates before the epoch parse to negative seconds rather than wrapping. The
/// era arithmetic has to floor for negative years, which is the same reason the
/// formatter uses `div_euclid`.
#[test]
fn dates_before_the_epoch_parse_negative() {
	assert_eq!(parse_rfc3339("1969-12-31T23:59:59Z"), Some(-1));
	assert_eq!(parse_rfc3339("1969-12-31T00:00:00Z"), Some(-86_400));
}

/// A date that does not exist is rejected rather than silently rolling into the
/// next month. This is the check that makes the cell blank instead of showing a
/// plausible wrong day.
#[test]
fn impossible_dates_are_rejected() {
	assert_eq!(parse_rfc3339("2026-02-30T00:00:00Z"), None);
	assert_eq!(
		parse_rfc3339("2026-02-29T00:00:00Z"),
		None,
		"2026 is not a leap year"
	);
	assert_eq!(parse_rfc3339("2026-04-31T00:00:00Z"), None);
	assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
	assert_eq!(parse_rfc3339("2026-00-01T00:00:00Z"), None);
	assert_eq!(parse_rfc3339("2026-01-00T00:00:00Z"), None);
	assert_eq!(parse_rfc3339("2026-01-01T24:00:00Z"), None);
	assert_eq!(parse_rfc3339("2026-01-01T00:60:00Z"), None);
}

/// The leap-year rule has to be the full one. 2024 is a leap year, 2100 is not
/// despite being divisible by four, and 2000 is despite being a century.
#[test]
fn the_leap_year_rule_is_the_whole_rule() {
	assert!(parse_rfc3339("2024-02-29T00:00:00Z").is_some());
	assert!(parse_rfc3339("2000-02-29T00:00:00Z").is_some());
	assert!(parse_rfc3339("2100-02-29T00:00:00Z").is_none());
	assert!(parse_rfc3339("1900-02-29T00:00:00Z").is_none());
}

/// Malformed input returns `None` rather than a wrong instant. Every one of
/// these is a shape that a laxer parser accepts: `str::parse` takes a leading
/// sign and leading spaces, a missing zone would default to UTC, and a stray
/// suffix would be ignored.
#[test]
fn malformed_input_is_refused() {
	for bad in [
		"",
		"not a timestamp",
		"2026-05-07T17:41:42",          // no zone at all
		"2026-05-07T17:41:42.",         // a dot with no digits
		"2026-05-07T17:41:42.Z",        // same, with a zone after it
		"2026-05-07T17:41:42Zextra",    // trailing junk after Z
		"2026-05-07T17:41:42+05",       // truncated offset
		"2026-05-07T17:41:42+0500",     // offset with no colon
		"2026-05-07T17:41:42+05:00:00", // offset with seconds
		"2026-05-07T17:41:42+99:00",    // offset out of range
		"2026-05-07T17:41:42+05:99",    // offset minutes out of range
		"20 6-05-07T17:41:42Z",         // a space where a digit belongs
		"+026-05-07T17:41:42Z",         // a sign where a digit belongs
		"2026/05/07T17:41:42Z",         // wrong date separators
		"2026-05-07X17:41:42Z",         // wrong date/time separator
		"2026-05-07T17-41-42Z",         // wrong time separators
	] {
		assert_eq!(parse_rfc3339(bad), None, "{bad:?} should not parse");
	}
}

/// The separator may be `T`, lowercase `t`, or a space. RFC 3339 permits all
/// three, and a user typing a `--since` by hand reaches for the space.
#[test]
fn every_permitted_separator_is_accepted() {
	let expected = parse_rfc3339("2026-05-07T17:41:42Z");
	assert!(expected.is_some());
	assert_eq!(parse_rfc3339("2026-05-07t17:41:42Z"), expected);
	assert_eq!(parse_rfc3339("2026-05-07 17:41:42Z"), expected);
	assert_eq!(parse_rfc3339("2026-05-07T17:41:42z"), expected);
}

/// A leap second is folded onto the following second rather than refused. The
/// server can legitimately send `:60`, and blanking the cell over it would lose
/// a timestamp that is very nearly right.
#[test]
fn a_leap_second_is_accepted() {
	let ordinary = parse_rfc3339("2016-12-31T23:59:59Z").unwrap();
	assert_eq!(parse_rfc3339("2016-12-31T23:59:60Z"), Some(ordinary + 1));
}

/// The fractional part is validated and discarded, whatever its length. Every
/// consumer renders whole seconds at coarsest.
#[test]
fn the_fractional_part_is_ignored_at_any_length() {
	let whole = parse_rfc3339("2026-05-07T17:41:42Z");
	assert_eq!(parse_rfc3339("2026-05-07T17:41:42.9Z"), whole);
	assert_eq!(parse_rfc3339("2026-05-07T17:41:42.866453985Z"), whole);
	assert_eq!(parse_rfc3339("2026-05-07T17:41:42.000000000000Z"), whole);
}
