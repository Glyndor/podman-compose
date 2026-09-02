use super::validate_signal;
use crate::error::ComposeError;

#[test]
fn accepts_common_signal_names() {
	for s in ["SIGKILL", "SIGTERM", "SIGHUP", "SIGINT", "SIGUSR1"] {
		assert!(validate_signal(s).is_ok(), "{s} should be accepted");
	}
}

#[test]
fn accepts_names_without_sig_prefix_case_insensitive() {
	assert!(validate_signal("TERM").is_ok());
	assert!(validate_signal("term").is_ok());
	assert!(validate_signal("Kill").is_ok());
}

#[test]
fn accepts_numeric_signals_in_range() {
	assert!(validate_signal("9").is_ok());
	assert!(validate_signal("15").is_ok());
	assert!(validate_signal("1").is_ok());
	assert!(validate_signal("64").is_ok());
}

#[test]
fn rejects_empty_signal() {
	// The core bug: an empty signal must not be forwarded (it would default
	// to SIGKILL on the libpod side).
	let err = validate_signal("").unwrap_err();
	assert!(matches!(err, ComposeError::InvalidSignal(_)));
	assert!(err.to_string().contains("invalid signal"));
}

#[test]
fn rejects_whitespace_only_signal() {
	assert!(matches!(
		validate_signal("   ").unwrap_err(),
		ComposeError::InvalidSignal(_)
	));
}

#[test]
fn rejects_out_of_range_and_zero_numbers() {
	assert!(matches!(
		validate_signal("0").unwrap_err(),
		ComposeError::InvalidSignal(_)
	));
	assert!(matches!(
		validate_signal("65").unwrap_err(),
		ComposeError::InvalidSignal(_)
	));
	assert!(matches!(
		validate_signal("9999").unwrap_err(),
		ComposeError::InvalidSignal(_)
	));
}

#[test]
fn rejects_unknown_signal_names() {
	assert!(matches!(
		validate_signal("SIGBOGUS").unwrap_err(),
		ComposeError::InvalidSignal(_)
	));
	assert!(matches!(
		validate_signal("not-a-signal").unwrap_err(),
		ComposeError::InvalidSignal(_)
	));
}
