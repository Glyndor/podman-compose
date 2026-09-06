use super::cli_logs_tail_default;

/// The CLI default is bounded. Without `--tail`, the dispatch substitutes
/// the constant so the wire query carries `&tail=100`. Library callers that
/// build `LogsOptions` directly keep `None` (= all); see the issue.
#[test]
fn cli_logs_tail_default_substitutes_the_bounded_default_when_missing() {
	assert_eq!(cli_logs_tail_default(None).as_deref(), Some("100"));
}

/// `--tail all` and `--tail <N>` are user intent and pass through unchanged.
#[test]
fn cli_logs_tail_default_preserves_an_explicit_value() {
	assert_eq!(
		cli_logs_tail_default(Some("all".into())).as_deref(),
		Some("all")
	);
	assert_eq!(
		cli_logs_tail_default(Some("42".into())).as_deref(),
		Some("42")
	);
}
