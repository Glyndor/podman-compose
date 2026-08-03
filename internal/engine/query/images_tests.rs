use super::size_cell;

/// The exact strings the reference tools printed for these byte counts on
/// 2026-08-03: `docker compose` v5.1.3 rendered `98.2MB` for `redis:8-alpine`,
/// and `podman images` rendered `1.01 GB` and `805 kB` on the same host. The
/// table exists to be compared against theirs, so a divergence here is a bug in
/// this column rather than a matter of taste.
#[test]
fn the_size_cell_matches_the_reference_tools() {
	assert_eq!(size_cell(98_234_179), "98.2MB");
	assert_eq!(size_cell(805_007), "805kB");
	assert_eq!(size_cell(1_010_000_000), "1.01GB");
}

/// Decimal, not binary. The same image renders 5% smaller under the binary
/// ladder, and a reader diffing this table against `podman images` would see
/// every row disagree.
#[test]
fn the_size_cell_uses_the_decimal_ladder() {
	// 98234179 bytes is 98.2 MB decimal and 93.7 MiB binary.
	let cell = size_cell(98_234_179);
	assert!(
		cell.ends_with("MB"),
		"{cell:?} is not on the decimal ladder"
	);
	assert!(!cell.contains("iB"), "{cell:?} used a binary unit");
}

/// An image that is not present locally reports zero, and zero is not a size —
/// it is the absence of an answer. An empty cell says that; `0B` would claim
/// podup asked and the image really is empty.
#[test]
fn a_missing_image_leaves_the_cell_empty() {
	assert_eq!(size_cell(0), "");
}

/// One byte is a real size and renders as one, so the empty cell above is
/// keyed on "no answer" and not on "small".
#[test]
fn a_one_byte_image_still_renders() {
	assert_eq!(size_cell(1), "1B");
}
