//! Tests for the legacy `PODUP_LIBCOD_POOL` -> `PODUP_LIBPOD_POOL` env
//! bridge. Kept in its own file so `tests.rs` stays near its existing size;
//! `tests.rs` was already past the 300-line soft warn when this branch
//! forked, and the bridge is the only new suite being added.

// Helper: run `bridge_legacy_pool_env` with `PODUP_LIBPOD_POOL` and
// `PODUP_LIBCOD_POOL` set to the given `(Option<&str>, Option<&str>)`
// and return what the new var reads after. The new var is captured in
// and out: the helper both removes it before and inspects it after, so
// a test asserts on the post-call observation only.
fn run_bridge(new: Option<&str>, legacy: Option<&str>) -> Option<String> {
	// Clear both to start. A leftover `PODUP_LIBPOD_POOL` from a
	// parent process would otherwise leak into the test.
	std::env::remove_var("PODUP_LIBPOD_POOL");
	std::env::remove_var("PODUP_LIBCOD_POOL");
	if let Some(v) = new {
		std::env::set_var("PODUP_LIBPOD_POOL", v);
	}
	if let Some(v) = legacy {
		std::env::set_var("PODUP_LIBCOD_POOL", v);
	}
	super::bridge_legacy_pool_env();
	let after = std::env::var("PODUP_LIBPOD_POOL").ok();
	std::env::remove_var("PODUP_LIBPOD_POOL");
	std::env::remove_var("PODUP_LIBCOD_POOL");
	after
}

#[test]
fn bridge_does_nothing_when_neither_var_is_set() {
	// The early-return path: with neither env present the helper is
	// a no-op and `PODUP_LIBPOD_POOL` stays unset after the call.
	assert_eq!(run_bridge(None, None), None);
}

#[test]
fn bridge_copies_legacy_into_the_new_var_when_new_is_unset() {
	// The contract a script that exports only `PODUP_LIBCOD_POOL`
	// depends on: after the call the new var reads the old value.
	assert_eq!(
		run_bridge(None, Some("4")),
		Some("4".to_string()),
		"legacy value must be visible to clap under the new name",
	);
}

#[test]
fn bridge_leaves_an_explicit_new_var_untouched() {
	// Set precedence: a script that exports BOTH wins when its new-var
	// value is the explicit one; the legacy value must not overwrite.
	assert_eq!(
		run_bridge(Some("8"), Some("0")),
		Some("8".to_string()),
		"new var must not be overwritten when explicitly exported",
	);
}
