//! The reader's UTC offset on Unix, from libc.
//!
//! Rust's standard library has no local time, deliberately: resolving one needs
//! a timezone database, and the C function that reads the system's has a
//! documented race. podup already depends on `libc` and already carries FFI in
//! five other modules, so this is that pattern rather than a new dependency or
//! an in-tree tzfile parser.

// libc FFI (localtime_r) is needed here; the block carries a soundness comment.
#![allow(unsafe_code)]

/// Offset from UTC in seconds at `unix_secs`, or `None` when libc refuses.
///
/// Resolved per instant rather than once, so an instant on the far side of a
/// daylight-saving transition gets the offset that applied then.
pub(super) fn local_offset_seconds(unix_secs: i64) -> Option<i64> {
	let time = libc::time_t::try_from(unix_secs).ok()?;
	let mut tm: libc::tm = unsafe { std::mem::zeroed() };

	// SAFETY: `localtime_r` writes a `struct tm` through the pointer it is
	// given and reads nothing else of ours. `tm` is a live, correctly typed,
	// exclusively borrowed local; `&time` is a live `time_t`. The reentrant
	// form is used precisely because it writes to the caller's buffer instead
	// of a shared static, so nothing here can be clobbered by another thread.
	//
	// The known hazard with this family is `tzset`, which reads the `TZ`
	// environment variable and races a concurrent `setenv`. podup never sets an
	// environment variable at runtime — the only writes are in test processes,
	// and no test calls this — so the race has no second party. A returned null
	// means libc could not resolve a zone at all and is handled rather than
	// assumed away.
	let result = unsafe { libc::localtime_r(&time, &mut tm) };
	if result.is_null() {
		return None;
	}
	Some(tm.tm_gmtoff as i64)
}
