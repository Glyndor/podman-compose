use super::{format_duration, DurationFormat};
use std::time::Duration;

/// The ladder is walked in `u128` nanoseconds, which `Duration::MAX` cannot
/// exceed, so the largest span the type can hold still renders in its own units
/// instead of saturating into an ever-growing day count.
#[test]
fn the_ladder_covers_the_whole_input_type() {
	assert_eq!(
		format_duration(Duration::MAX, &DurationFormat::Parts(3)),
		"584942417355y 26d 7h"
	);
	assert_eq!(
		format_duration(Duration::MAX, &DurationFormat::Parts(8)),
		"584942417355y 26d 7h 15s 999ms 999µs 999ns"
	);
}

/// The owner's worked example. It has no minutes, and the minutes are absent
/// from the output for the same reason months are absent from the ladder: a
/// component that carries nothing is not printed.
#[test]
fn composite_renders_the_example_from_the_issue() {
	let span = Duration::new(31_626_004, 5_000_000);
	assert_eq!(
		format_duration(span, &DurationFormat::Parts(5)),
		"1y 1d 1h 4s 5ms"
	);
}

/// Three components is the default. The same span at five components is what
/// the default exists to cut off.
#[test]
fn the_default_is_three_components() {
	let span = Duration::new(7503, 200_000_000);
	assert_eq!(
		format_duration(span, &DurationFormat::default_parts()),
		"2h 5m 3s"
	);
	assert_eq!(
		format_duration(span, &DurationFormat::Parts(5)),
		"2h 5m 3s 200ms"
	);
}

/// The component count is fixed and the units that fill it slide with the
/// magnitude. This is the whole contract: a column three components wide always
/// carries the three that matter at that scale, so a freshly started container
/// and one up for two years are equally readable in the same width.
#[test]
fn the_window_slides_up_the_ladder_as_the_span_grows() {
	let three = DurationFormat::default_parts();
	let day = 86_400;
	for (secs, expected) in [
		(5_u64, "5s"),
		(90, "1m 30s"),
		(3903, "1h 5m 3s"),
		(61 * day, "2mo 1d"),
		(400 * day, "1y 1mo 5d"),
		// A gap inside the window: a year and two days has no whole month in
		// it, so the month is skipped rather than ending the walk. Without this
		// row the whole test passes against a formatter that stops at the first
		// empty unit, because every span above happens to have none.
		(367 * day, "1y 2d"),
		(365 * day + 3600, "1y 1h"),
	] {
		assert_eq!(
			format_duration(Duration::from_secs(secs), &three),
			expected,
			"{secs}s"
		);
	}
}

/// A span smaller than its component count renders only what it has, with no
/// `0s` padding to fill the request.
#[test]
fn composite_stops_when_the_span_runs_out() {
	assert_eq!(
		format_duration(Duration::from_millis(340), &DurationFormat::Parts(4)),
		"340ms"
	);
	assert_eq!(
		format_duration(Duration::from_nanos(1), &DurationFormat::Parts(4)),
		"1ns"
	);
}

/// Every rung of the ladder, at the exact span that first reaches it, so a unit
/// cannot be silently skipped or mislabelled.
#[test]
fn every_unit_appears_at_its_own_boundary() {
	let one = DurationFormat::Parts(1);
	assert_eq!(format_duration(Duration::from_nanos(1), &one), "1ns");
	assert_eq!(format_duration(Duration::from_micros(1), &one), "1µs");
	assert_eq!(format_duration(Duration::from_millis(1), &one), "1ms");
	assert_eq!(format_duration(Duration::from_secs(1), &one), "1s");
	assert_eq!(format_duration(Duration::from_secs(60), &one), "1m");
	assert_eq!(format_duration(Duration::from_secs(3600), &one), "1h");
	assert_eq!(format_duration(Duration::from_secs(86_400), &one), "1d");
	assert_eq!(format_duration(Duration::from_secs(2_592_000), &one), "1mo");
	assert_eq!(format_duration(Duration::from_secs(31_536_000), &one), "1y");
}

/// A year is exactly 365 days and a month exactly 30, both pinned because a
/// reader doing the arithmetic back has to land on the same number.
#[test]
fn a_year_is_three_hundred_and_sixty_five_days_and_a_month_is_thirty() {
	let one = DurationFormat::Parts(1);
	assert_eq!(
		format_duration(Duration::from_secs(30 * 86_400), &one),
		"1mo"
	);
	assert_eq!(
		format_duration(Duration::from_secs(365 * 86_400), &one),
		"1y"
	);
}

/// The two definitions do not divide evenly, and the seam is deliberate rather
/// than a rounding slip: twelve thirty-day months are 360 days, so 364 days is
/// twelve months and four days while 365 is one year. The alternative is a
/// fractional month nobody can verify by hand.
#[test]
fn twelve_months_are_not_a_year_and_the_output_says_so() {
	let three = DurationFormat::default_parts();
	assert_eq!(
		format_duration(Duration::from_secs(364 * 86_400), &three),
		"12mo 4d"
	);
	assert_eq!(
		format_duration(Duration::from_secs(365 * 86_400), &three),
		"1y"
	);
}

/// Plain milliseconds is one number and one unit. Its decimals are what keep it
/// from having a cliff of its own: a benchmark row of 340 microseconds would
/// read `0ms` as a whole number, which is the wrong answer rather than a
/// coarse one.
#[test]
fn plain_milliseconds_keeps_sub_millisecond_spans() {
	let two = DurationFormat::Millis { decimals: 2 };
	assert_eq!(format_duration(Duration::from_micros(340), &two), "0.34ms");
	assert_eq!(
		format_duration(Duration::from_millis(340), &two),
		"340.00ms"
	);
	assert_eq!(format_duration(Duration::from_secs(90), &two), "90000.00ms");
}

/// The decimal count is the caller's, the same way it is for sizes.
#[test]
fn the_millisecond_decimal_count_is_configurable() {
	let span = Duration::from_micros(1_234_567);
	assert_eq!(
		format_duration(span, &DurationFormat::Millis { decimals: 0 }),
		"1235ms"
	);
	assert_eq!(
		format_duration(span, &DurationFormat::Millis { decimals: 3 }),
		"1234.567ms"
	);
}

/// Zero has no components and still has to render something. `0s` under either
/// shape, never an empty cell.
#[test]
fn zero_renders_under_either_shape() {
	assert_eq!(
		format_duration(Duration::ZERO, &DurationFormat::Parts(3)),
		"0s"
	);
	assert_eq!(
		format_duration(Duration::ZERO, &DurationFormat::Millis { decimals: 2 }),
		"0.00ms"
	);
}

/// A zero component count is a caller mistake with no sensible rendering, so it
/// falls back to one component rather than returning an empty string.
#[test]
fn a_zero_component_count_still_renders_one_component() {
	assert_eq!(
		format_duration(Duration::from_secs(3661), &DurationFormat::Parts(0)),
		"1h"
	);
}
