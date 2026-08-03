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

/// Round-trip against the formatter that already lives in the tree. This is the
/// test the two functions exist to give each other: `format_event_time` walks
/// civil-from-days and this walks days-from-civil, so for both to agree and both
/// be wrong they would have to be wrong in the same direction.
#[test]
fn it_round_trips_against_the_event_formatter() {
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
		1_785_718_245, // the instant #1301 was built for
		4_102_444_800, // 2100-01-01, a century that is not a leap year
	] {
		let printed = crate::engine::events::format_event_time(unix);
		let reparsed = parse_rfc3339(&format!("{printed}Z"))
			.unwrap_or_else(|| panic!("could not reparse {printed:?} (from {unix})"));
		assert_eq!(reparsed, unix, "{printed:?} did not round-trip");
	}
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
