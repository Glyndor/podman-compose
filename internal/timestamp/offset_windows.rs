//! The reader's UTC offset on Windows, from the Win32 time API.
//!
//! `SystemTimeToTzSpecificLocalTime` rather than `GetTimeZoneInformation`'s
//! bias: the bias describes the zone *now*, while this converts a specific
//! instant and so applies the daylight rule that was in force then. An events
//! feed is mostly live, but a `--since` window can reach across a transition,
//! and a formatter that is right only for today is the kind of defect nobody
//! notices until October.

// Win32 FFI is needed here; each block carries a soundness comment.
#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::SYSTEMTIME;
use windows_sys::Win32::System::Time::SystemTimeToTzSpecificLocalTime;

/// Offset from UTC in seconds at `unix_secs`, or `None` when Windows refuses.
pub(super) fn local_offset_seconds(unix_secs: i64) -> Option<i64> {
	let utc = super::to_system_time(unix_secs)?;
	let mut local = SYSTEMTIME {
		wYear: 0,
		wMonth: 0,
		wDayOfWeek: 0,
		wDay: 0,
		wHour: 0,
		wMinute: 0,
		wSecond: 0,
		wMilliseconds: 0,
	};

	// SAFETY: both pointers are to live, correctly typed, exclusively borrowed
	// locals for the duration of the call, and the function writes only through
	// the out parameter. A null first argument asks for the machine's current
	// time zone, which is what the reader's wall clock means here. The call is
	// documented to return zero on failure, which is handled rather than
	// assumed away.
	let ok = unsafe { SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) };
	if ok == 0 {
		return None;
	}
	// The difference of the two wall clocks *is* the offset: the same instant
	// expressed twice. Computing it this way rather than reading a bias field
	// means the daylight rule is applied by Windows for that instant, not
	// re-implemented here.
	Some(super::from_system_time(&local) - unix_secs)
}
