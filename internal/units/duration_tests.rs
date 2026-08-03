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

/// Two components is the default because that is what reads at a glance. The
/// same span at five components is the thing the default exists to avoid.
#[test]
fn the_default_is_two_components() {
	let span = Duration::new(7503, 200_000_000);
	assert_eq!(
		format_duration(span, &DurationFormat::default_parts()),
		"2h 5m"
	);
	assert_eq!(
		format_duration(span, &DurationFormat::Parts(5)),
		"2h 5m 3s 200ms"
	);
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
	assert_eq!(format_duration(Duration::from_secs(31_536_000), &one), "1y");
}

/// A year is exactly 365 days, so 365 days is one year and 364 is not. The
/// definition is arbitrary but it has to be pinned: a reader doing the
/// arithmetic back has to land on the same number.
#[test]
fn a_year_is_three_hundred_and_sixty_five_days() {
	let one = DurationFormat::Parts(1);
	assert_eq!(
		format_duration(Duration::from_secs(364 * 86_400), &one),
		"364d"
	);
	assert_eq!(
		format_duration(Duration::from_secs(365 * 86_400), &one),
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
