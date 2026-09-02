use super::reject_dangerous_secret_mode;

#[test]
fn data_modes_accepted() {
	// A secret holds data: read/write owner bits and the world-readable
	// default are all fine for a native secret.
	assert!(reject_dangerous_secret_mode(0o400, "s").is_ok());
	assert!(reject_dangerous_secret_mode(0o600, "s").is_ok());
	assert!(reject_dangerous_secret_mode(0o444, "s").is_ok());
}

#[test]
fn execute_setuid_setgid_sticky_rejected() {
	for mode in [0o100, 0o500, 0o700, 0o4000, 0o2000, 0o1000] {
		assert!(
			reject_dangerous_secret_mode(mode, "s").is_err(),
			"{mode:#o} must be rejected"
		);
	}
}
