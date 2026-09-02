use super::width_from;

/// `window_size` answers `(rows, cols)`. Getting this backwards is invisible
/// on a square-ish terminal and mangles every line on a normal one.
#[test]
fn the_width_is_the_columns_not_the_rows() {
	assert_eq!(width_from(Some((30, 100))), Some(100));
	assert_eq!(width_from(None), None);
}
