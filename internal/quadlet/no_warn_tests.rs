//! The Quadlet-path host-binding / privilege-escalation warnings are
//! gated on a thread-local set by [`NoWarnGuard`]. The guard restores the
//! previous value on drop so nested scopes compose; a test that runs
//! inside another test's guard (or after one has leaked) would see a
//! different starting value, so the guard is documented as zero-overhead
//! for callers that do not wrap `write_quadlet`.
use super::{is_no_warn_set, NoWarnGuard};

#[test]
fn no_warn_is_off_by_default() {
	assert!(!is_no_warn_set());
}

#[test]
fn guard_sets_and_restores() {
	assert!(!is_no_warn_set());
	{
		let _g = NoWarnGuard::new();
		assert!(is_no_warn_set());
	}
	assert!(
		!is_no_warn_set(),
		"dropping the guard must restore the previous value"
	);
}

#[test]
fn nested_guards_restore_in_reverse_order() {
	let _outer = NoWarnGuard::new();
	assert!(is_no_warn_set());
	{
		// A nested guard inherits the inner value (true), so dropping it
		// leaves the outer guard's setting intact.
		let _inner = NoWarnGuard::new();
	}
	assert!(
		is_no_warn_set(),
		"a nested NoWarnGuard must not stomp the outer guard's value"
	);
}
