use super::{format_bytes, SizeBase, SizeFormat, SizeShape};

/// Both ladders reach an exponent `u64::MAX` cannot exceed, so no input can
/// push the renderer off the end of its table. This is the whole reason the
/// units go past what a container plausibly reports: a formatter is either
/// total over its input type or it has a cliff, and the cliff is only ever
/// found by whoever is big enough to hit it.
#[test]
fn both_ladders_cover_the_whole_input_type() {
	assert_eq!(format_bytes(u64::MAX, &SizeFormat::binary()), "16.00EiB");
	assert_eq!(format_bytes(u64::MAX, &SizeFormat::decimal()), "18.45EB");
}

/// The regression the ladder was extended for: stopping at `TiB` rendered a
/// petabyte as `1024.0TiB` — right arithmetic, wrong unit, and nine digits in a
/// column sized for five.
#[test]
fn a_petabyte_does_not_saturate_into_terabytes() {
	assert_eq!(
		format_bytes(1024_u64.pow(5), &SizeFormat::binary()),
		"1.00PiB"
	);
	assert_eq!(
		format_bytes(1000_u64.pow(5), &SizeFormat::decimal()),
		"1.00PB"
	);
}

/// Every rung of the binary ladder, at the exact value that first reaches it.
#[test]
fn binary_units_step_at_their_own_boundaries() {
	let fmt = SizeFormat::binary();
	assert_eq!(format_bytes(1024, &fmt), "1.00KiB");
	assert_eq!(format_bytes(1024_u64.pow(2), &fmt), "1.00MiB");
	assert_eq!(format_bytes(1024_u64.pow(3), &fmt), "1.00GiB");
	assert_eq!(format_bytes(1024_u64.pow(4), &fmt), "1.00TiB");
	assert_eq!(format_bytes(1024_u64.pow(5), &fmt), "1.00PiB");
	assert_eq!(format_bytes(1024_u64.pow(6), &fmt), "1.00EiB");
}

/// Every rung of the decimal ladder. This is the base podman and docker print,
/// so an `images` or `ps` table is compared against these strings, not the
/// binary ones.
#[test]
fn decimal_units_step_at_their_own_boundaries() {
	let fmt = SizeFormat::decimal();
	// Lowercase k, the SI prefix, matching what podman prints.
	assert_eq!(format_bytes(1000, &fmt), "1.00kB");
	assert_eq!(format_bytes(1000_u64.pow(2), &fmt), "1.00MB");
	assert_eq!(format_bytes(1000_u64.pow(3), &fmt), "1.00GB");
	assert_eq!(format_bytes(1000_u64.pow(4), &fmt), "1.00TB");
	assert_eq!(format_bytes(1000_u64.pow(5), &fmt), "1.00PB");
	assert_eq!(format_bytes(1000_u64.pow(6), &fmt), "1.00EB");
}

/// The two bases disagree on the same input, which is why the base is a
/// per-surface choice rather than a constant. 8_711_starting bytes is the
/// worked case from the issue: podman says `8.71MB`, `free` says `8.31MiB`.
#[test]
fn the_two_bases_render_the_same_input_differently() {
	assert_eq!(format_bytes(8_710_000, &SizeFormat::decimal()), "8.71MB");
	assert_eq!(format_bytes(8_710_000, &SizeFormat::binary()), "8.31MiB");
}

/// Under a kilobyte there is no fraction of a byte to report, so the value
/// prints whole under either base and costs no column width.
#[test]
fn whole_bytes_carry_no_decimals() {
	assert_eq!(format_bytes(0, &SizeFormat::binary()), "0B");
	assert_eq!(format_bytes(1, &SizeFormat::binary()), "1B");
	assert_eq!(format_bytes(1023, &SizeFormat::binary()), "1023B");
	assert_eq!(format_bytes(999, &SizeFormat::decimal()), "999B");
}

/// A value just under a boundary rounds up onto it, and the renderer promotes
/// rather than printing `1024.00KiB`. Without the promotion this is the same
/// class of defect as saturating at `TiB`, one rung lower and much easier to
/// hit: any size within half a display digit of a boundary shows it.
#[test]
fn a_value_that_rounds_onto_the_next_unit_is_promoted() {
	assert_eq!(
		format_bytes(1024_u64.pow(2) - 1, &SizeFormat::binary()),
		"1.00MiB"
	);
	assert_eq!(
		format_bytes(1000_u64.pow(2) - 1, &SizeFormat::decimal()),
		"1.00MB"
	);
	// One rung further up, to show the promotion is not special-cased to KiB.
	assert_eq!(
		format_bytes(1024_u64.pow(4) - 1, &SizeFormat::binary()),
		"1.00TiB"
	);
}

/// Below the rounding threshold the value stays on its own rung, which is what
/// makes the test above a promotion rather than an off-by-one.
#[test]
fn a_value_that_does_not_round_up_stays_on_its_rung() {
	// 1048000 bytes is 1023.4KiB, which rounds to 1023.44 — still KiB.
	assert_eq!(format_bytes(1_048_000, &SizeFormat::binary()), "1023.44KiB");
}

/// The decimal count is the caller's, because a table cell and a summary line
/// have different width budgets.
#[test]
fn the_decimal_count_is_configurable() {
	let bytes = 1_610_612_736; // 1.5 GiB exactly.
	assert_eq!(
		format_bytes(bytes, &SizeFormat::binary().with_decimals(0)),
		"2GiB"
	);
	assert_eq!(
		format_bytes(bytes, &SizeFormat::binary().with_decimals(1)),
		"1.5GiB"
	);
	assert_eq!(
		format_bytes(bytes, &SizeFormat::binary().with_decimals(4)),
		"1.5000GiB"
	);
}

/// The reference rendering, captured rather than assumed. Every value here was
/// read off a real tool on 2026-08-03: `docker compose` v5.1.3 printed `98.2MB`
/// for `redis:8-alpine`, and `podman images` printed `1.01 GB`, `101 MB` and
/// `805 kB` on the same host. Three digits in all of them, which a fixed decimal
/// count cannot produce — `98.2` has one decimal and `8.71` has two.
#[test]
fn significant_digits_match_what_podman_and_docker_print() {
	let fmt = SizeFormat::decimal().with_significant(3);
	assert_eq!(format_bytes(98_234_179, &fmt), "98.2MB");
	assert_eq!(format_bytes(8_710_000, &fmt), "8.71MB");
	assert_eq!(format_bytes(805_007, &fmt), "805kB");
	assert_eq!(format_bytes(1_010_000_000, &fmt), "1.01GB");
}

/// The digit count holds across every magnitude, which is the property that
/// keeps the column from breathing as rows scroll past. A fixed decimal count
/// gives `1.00GB` and `999.00MB` — four characters apart.
#[test]
fn significant_digits_keep_a_constant_width() {
	let fmt = SizeFormat::decimal().with_significant(3);
	for (bytes, expected) in [
		(1_000_u64, "1.00kB"),
		(12_000, "12.0kB"),
		(123_000, "123kB"),
		(1_230_000, "1.23MB"),
		(123_000_000_000_000_000, "123PB"),
	] {
		let rendered = format_bytes(bytes, &fmt);
		assert_eq!(rendered, expected);
		let digits = rendered.chars().filter(char::is_ascii_digit).count();
		assert_eq!(digits, 3, "{rendered:?} is not three digits wide");
	}
}

/// A promotion changes the value's magnitude, so the digit count has to be
/// recomputed for the new rung. Without that, 999999 bytes rounds to `1000kB`
/// and then renders as `1kB` — the promotion applied and the decimals did not.
#[test]
fn significant_digits_are_recomputed_after_a_promotion() {
	let fmt = SizeFormat::decimal().with_significant(3);
	assert_eq!(format_bytes(999_999, &fmt), "1.00MB");
	assert_eq!(format_bytes(999_999_999, &fmt), "1.00GB");
}

/// Whole bytes stay whole under this shape too: there is no fraction of a byte,
/// so a three-digit request cannot invent one.
#[test]
fn significant_digits_do_not_reach_below_a_byte() {
	let fmt = SizeFormat::decimal().with_significant(3);
	assert_eq!(format_bytes(0, &fmt), "0B");
	assert_eq!(format_bytes(7, &fmt), "7B");
	assert_eq!(format_bytes(999, &fmt), "999B");
}

/// A zero-digit request still shows a digit, because `{:.0}` prints one
/// whatever it is handed. Nothing in the formatter enforces this — it is a
/// property of the format machinery, and it is pinned here so a future clamp
/// added "to be safe" has something to justify itself against.
#[test]
fn a_zero_digit_request_still_renders_a_digit() {
	let fmt = SizeFormat::decimal().with_significant(0);
	assert_eq!(format_bytes(8_710_000, &fmt), "9MB");
}

/// Tested one level below `format_bytes` on purpose. Unit selection guarantees
/// the value handed over is at least one, so the sub-one branch cannot be
/// reached through the public entry point — a mutation deleting the clamp that
/// used to sit here survived every test that went in the front door, which is
/// what dead defensive code looks like from outside. The clamp is gone; this
/// pins what the arithmetic actually does, including where it is weak.
#[test]
fn the_decimal_count_is_pinned_below_its_public_entry_point() {
	use super::Precision;
	// The ordinary path, which is everything `format_bytes` can produce.
	assert_eq!(Precision::Significant(3).decimals_for(98.2), 1);
	assert_eq!(Precision::Significant(3).decimals_for(8.71), 2);
	assert_eq!(Precision::Significant(3).decimals_for(805.0), 0);
	assert_eq!(Precision::Fixed(4).decimals_for(805.0), 4);
	// Sub-one values count their leading zero as the digit before the point.
	// That means one significant digit of 0.5 asks for no decimals and renders
	// `0` — a real weakness, and unreachable, so it is recorded rather than
	// fixed. If a caller ever hands this a fraction, fix it then.
	assert_eq!(Precision::Significant(3).decimals_for(0.5), 2);
	assert_eq!(Precision::Significant(1).decimals_for(0.5), 0);
}

/// The owner's worked examples, in the base each was written in.
#[test]
fn composite_renders_the_examples_from_the_issue() {
	let decimal = SizeFormat::decimal().with_parts(2);
	assert_eq!(format_bytes(1_001_000_000_000, &decimal), "1TB 1GB");
	assert_eq!(format_bytes(1_512_000_000, &decimal), "1GB 512MB");
}

/// Components that are zero are skipped rather than ending the walk, so the
/// parts that survive are the ones carrying value. A terabyte and five
/// megabytes has nothing in between, and reporting `1TiB` alone would drop the
/// five megabytes the caller asked for a second component to see.
#[test]
fn composite_skips_empty_components() {
	let fmt = SizeFormat::binary().with_parts(2);
	let bytes = 1024_u64.pow(4) + 5 * 1024_u64.pow(2);
	assert_eq!(format_bytes(bytes, &fmt), "1TiB 5MiB");
}

/// Asking for more components than the value has yields only the ones that
/// exist — no `1KiB 0B` padding.
#[test]
fn composite_stops_when_the_value_runs_out() {
	let fmt = SizeFormat::binary().with_parts(7);
	assert_eq!(format_bytes(1024, &fmt), "1KiB");
	assert_eq!(format_bytes(1025, &fmt), "1KiB 1B");
}

/// Composite arithmetic is integer division, so the components sum back to the
/// input exactly. A float round-trip through seven rungs would not.
#[test]
fn composite_components_are_exact() {
	let fmt = SizeFormat::binary().with_parts(7);
	let bytes = u64::MAX;
	let rendered = format_bytes(bytes, &fmt);
	let units: Vec<(&str, u64)> = vec![
		("EiB", 1024_u64.pow(6)),
		("PiB", 1024_u64.pow(5)),
		("TiB", 1024_u64.pow(4)),
		("GiB", 1024_u64.pow(3)),
		("MiB", 1024_u64.pow(2)),
		("KiB", 1024),
		("B", 1),
	];
	let total: u64 = rendered
		.split(' ')
		.map(|part| {
			let (unit, scale) = units
				.iter()
				.find(|(unit, _)| part.ends_with(unit))
				.unwrap_or_else(|| panic!("unknown unit in {rendered:?}"));
			let count: u64 = part[..part.len() - unit.len()].parse().unwrap();
			count * scale
		})
		.sum();
	assert_eq!(total, bytes, "{rendered:?} did not sum back to the input");
}

/// Zero has no components at all, and still has to render something. Both
/// shapes answer `0B` rather than an empty cell.
#[test]
fn zero_renders_under_either_shape() {
	assert_eq!(format_bytes(0, &SizeFormat::binary().with_parts(3)), "0B");
	assert_eq!(format_bytes(0, &SizeFormat::decimal().with_parts(3)), "0B");
	assert_eq!(format_bytes(0, &SizeFormat::decimal()), "0B");
}

/// A zero component count is a caller mistake with no sensible rendering, so it
/// is treated as one component rather than returning an empty string.
#[test]
fn a_zero_component_count_still_renders_one_component() {
	assert_eq!(
		format_bytes(1025, &SizeFormat::binary().with_parts(0)),
		"1KiB"
	);
}

/// Three components by default, the same count durations use. One rule across
/// both formatters: a reader who has learned what `1h 5m 3s` means should not
/// have to learn a second one for a size.
#[test]
fn the_default_component_count_is_three_and_matches_durations() {
	use super::super::DurationFormat;
	let fmt = SizeFormat::decimal().default_parts();
	assert_eq!(fmt.shape, SizeShape::Composite { parts: 3 });
	assert_eq!(
		format_bytes(1_512_200_000, &fmt),
		"1GB 512MB 200kB",
		"three components, largest first"
	);
	// Read off the same constant, so the two cannot drift apart.
	assert_eq!(DurationFormat::default_parts(), DurationFormat::Parts(3));
}

/// Three is what a caller gets when it does not choose, not a floor on the
/// output: a value with fewer components renders only the ones it has.
#[test]
fn the_default_count_never_pads_a_short_value() {
	let fmt = SizeFormat::decimal().default_parts();
	assert_eq!(format_bytes(1000, &fmt), "1kB");
	assert_eq!(format_bytes(0, &fmt), "0B");
}

/// The builders replace the shape rather than merging into it, so the last call
/// wins and a caller cannot end up with a half-set combination.
#[test]
fn the_builders_replace_the_shape() {
	let fmt = SizeFormat::binary().with_decimals(3).with_parts(2);
	assert_eq!(fmt.shape, SizeShape::Composite { parts: 2 });
	assert_eq!(fmt.base, SizeBase::Binary);
	let fmt = SizeFormat::decimal().with_parts(2).with_decimals(3);
	assert_eq!(fmt.shape, SizeShape::Single { decimals: 3 });
	assert_eq!(fmt.base, SizeBase::Decimal);
}
