use super::*;

/// A non-terminal descriptor is declined rather than failing — the path
/// `podup exec` takes inside a pipeline, which must keep working.
///
/// Asked of an explicit `/dev/null` rather than of ambient stdin: the same
/// assertion on `enable()` would be testing the harness, not the code, and
/// would put a real terminal into raw mode on the way to failing.
#[test]
fn a_non_terminal_descriptor_is_declined() {
	let devnull = std::fs::File::open("/dev/null").expect("/dev/null opens");
	assert!(
		RawMode::enable_on(devnull.as_raw_fd()).is_none(),
		"a non-terminal descriptor must not be switched to raw mode"
	);
}

/// Likewise the size query: absence is a valid answer, not an error.
#[test]
fn a_non_terminal_descriptor_has_no_size() {
	let devnull = std::fs::File::open("/dev/null").expect("/dev/null opens");
	assert_eq!(size_of(devnull.as_raw_fd()), None);
}
