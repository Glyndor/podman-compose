use super::{parse_pull_policy, parse_scale_pair, parse_timeout};

#[test]
fn parse_scale_pair_accepts_valid() {
	assert_eq!(parse_scale_pair("web=3"), Ok(("web".to_string(), 3)));
}

#[test]
fn parse_scale_pair_rejects_bad_input() {
	// `web=+3` is rejected like the other malformed counts: `u32::FromStr`
	// tolerates a leading '+', so the explicit all-digits guard is what keeps
	// the contract consistent.
	for bad in [
		"web", "=3", "web=", "web=x", "web=0", "web=-1", "web=+3", "web=0x10", "web= 3",
	] {
		assert!(parse_scale_pair(bad).is_err(), "`{bad}` should be rejected");
	}
}

#[test]
fn parse_pull_policy_accepts_known_values() {
	for ok in ["always", "missing", "never", "newer", "build"] {
		assert_eq!(parse_pull_policy(ok), Ok(ok.to_string()));
	}
}

#[test]
fn parse_pull_policy_rejects_unknown_values() {
	for bad in ["bogus", "Always", "if_not_present", ""] {
		assert!(
			parse_pull_policy(bad).is_err(),
			"`{bad}` should be rejected"
		);
	}
}

#[test]
fn parse_timeout_accepts_zero_and_positive() {
	assert_eq!(parse_timeout("0"), Ok(0));
	assert_eq!(parse_timeout("30"), Ok(30));
}

#[test]
fn parse_timeout_rejects_negative_and_non_numeric() {
	assert!(parse_timeout("-5").is_err());
	assert!(parse_timeout("abc").is_err());
}
