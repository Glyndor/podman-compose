use super::*;

/// A non-console handle is declined rather than failing: the path
/// `podup exec` takes inside a pipeline, which must keep working.
///
/// Asked of an explicit `NUL` handle rather than of ambient stdin: the
/// same assertion on `enable()` would be testing how the harness was
/// invoked, and on the way to failing it would put a real console into
/// raw mode.
#[test]
fn a_non_console_handle_is_declined() {
	use std::os::windows::io::AsRawHandle;
	let devnull = std::fs::File::open("NUL").expect("NUL opens");
	let handle = devnull.as_raw_handle() as HANDLE;
	assert!(
		RawMode::enable_on(handle, handle).is_none(),
		"a non-console handle must not be switched to raw mode"
	);
}

/// Likewise the size query: absence is a valid answer, not an error.
#[test]
fn a_non_console_handle_has_no_size() {
	use std::os::windows::io::AsRawHandle;
	let devnull = std::fs::File::open("NUL").expect("NUL opens");
	assert_eq!(size_of(devnull.as_raw_handle() as HANDLE), None);
}

/// The window extent comes from `srWindow` and is inclusive on both ends;
/// `dwSize` (the scrollback buffer) must play no part in it.
#[test]
fn the_window_extent_is_the_visible_rectangle() {
	let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
	info.dwSize.X = 120;
	info.dwSize.Y = 9001; // scrollback: must not leak into the answer
	info.srWindow.Left = 0;
	info.srWindow.Right = 119;
	info.srWindow.Top = 8971;
	info.srWindow.Bottom = 9000;
	assert_eq!(window_extent(&info), Some((30, 120)));
}

/// A degenerate rectangle is unknown geometry, not a 0x0 pty.
#[test]
fn a_degenerate_window_has_no_size() {
	// A zeroed rectangle is a legal 1x1 window, so degeneracy has to be
	// forced: an inverted extent (Right < Left) yields a non-positive
	// width.
	let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
	info.srWindow.Right = -2;
	assert_eq!(window_extent(&info), None);
}

/// The poll dedup: only a *changed*, readable size is worth a resize call.
#[test]
fn resize_is_due_only_on_a_changed_readable_size() {
	let mut last = Some((24, 80));
	// Unchanged: nothing to do.
	assert_eq!(resize_due(&mut last, Some((24, 80))), None);
	// Unreadable (window lost): nothing to apply, and the last size is
	// kept so regaining the same one stays quiet.
	assert_eq!(resize_due(&mut last, None), None);
	assert_eq!(last, Some((24, 80)));
	// Changed: due, and recorded.
	assert_eq!(resize_due(&mut last, Some((30, 120))), Some((30, 120)));
	assert_eq!(last, Some((30, 120)));
	// The same change again: already reported.
	assert_eq!(resize_due(&mut last, Some((30, 120))), None);
}
