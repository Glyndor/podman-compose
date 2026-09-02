use super::*;

fn inspect(json: &str) -> ContainerInspect {
	serde_json::from_str(json).expect("fixture parses")
}

// --- wait_healthy poll plan (#418) ---------------------------------------

#[test]
fn poll_plan_defaults_match_legacy_60s() {
	// No healthcheck timing set → 2s between runs, 30 runs (60s budget).
	assert_eq!(
		super::health_poll_plan(None, None, None),
		(Duration::from_secs(2), Duration::from_secs(60))
	);
}

#[test]
fn poll_plan_uses_interval_and_honors_start_period() {
	// interval=10s, start_period=60s, retries=3 → run every 10s, budget
	// 3×10s + 60s.
	let (run, budget) = super::health_poll_plan(Some("10s"), Some("60s"), Some(3));
	assert_eq!(
		(run, budget),
		(Duration::from_secs(10), Duration::from_secs(90))
	);
}

/// A sub-second interval used to be discarded and replaced by the 2s
/// default, so asking for 500ms polling produced *slower* polling than
/// asking for 1s — the opposite of the request (#1147). It is honoured now,
/// with a floor so `10ms` cannot become a hundred check executions a second.
#[test]
fn poll_plan_honours_a_sub_second_interval() {
	let (run, _) = super::health_poll_plan(Some("500ms"), None, Some(5));
	assert_eq!(run, Duration::from_millis(500));
}

#[test]
fn poll_plan_floors_a_pathological_interval() {
	let (run, _) = super::health_poll_plan(Some("1ms"), None, Some(5));
	assert_eq!(run, super::MIN_RUN_INTERVAL);
}

/// Reading the status must never be slower than running the check, or the
/// fast observation path would be pointless.
#[test]
fn the_status_read_is_never_slower_than_the_default_run() {
	assert!(super::STATUS_READ_INTERVAL < Duration::from_secs(2));
}

// --- effective_budget (--wait-timeout, #891) -----------------------------

#[test]
fn budget_without_wait_timeout_uses_the_plan() {
	let plan = Duration::from_secs(60);
	assert_eq!(
		super::effective_budget(Duration::from_secs(2), plan, None),
		plan
	);
}

#[test]
fn budget_extends_to_cover_wait_timeout() {
	// A short plan must not cut a generous --wait-timeout short.
	let b = super::effective_budget(
		Duration::from_secs(10),
		Duration::from_secs(10),
		Some(Duration::from_secs(120)),
	);
	assert!(b > Duration::from_secs(120), "{b:?}");
}

#[test]
fn budget_keeps_the_larger_plan() {
	// A generous plan is not shortened by a small --wait-timeout.
	let b = super::effective_budget(
		Duration::from_secs(2),
		Duration::from_secs(200),
		Some(Duration::from_secs(10)),
	);
	assert_eq!(b, Duration::from_secs(200));
}

#[test]
fn health_reported_healthy() {
	let info = inspect(r#"{"State":{"Status":"running","Health":{"Status":"healthy"}}}"#);
	assert!(matches!(classify_health(&info), HealthVerdict::Healthy));
}

#[test]
fn health_no_effective_healthcheck_is_satisfied() {
	// A disabled healthcheck (Test ["NONE"]) can never report healthy, so the
	// dependency short-circuits as satisfied rather than blocking to timeout.
	let info =
		inspect(r#"{"State":{"Status":"running"},"Config":{"Healthcheck":{"Test":["NONE"]}}}"#);
	assert!(matches!(
		classify_health(&info),
		HealthVerdict::NoHealthcheck
	));
}

#[test]
fn health_starting_with_healthcheck_pends() {
	let info = inspect(
		r#"{"State":{"Status":"running","Health":{"Status":"starting"}},"Config":{"Healthcheck":{"Test":["CMD","true"]}}}"#,
	);
	assert!(matches!(classify_health(&info), HealthVerdict::Pending));
}

#[test]
fn health_exited_nonzero_fails() {
	// A no-healthcheck service that crashed during the wait must fail, not be
	// reported satisfied (the `up --wait` masking bug).
	let info = inspect(r#"{"State":{"Status":"exited","ExitCode":7}}"#);
	assert!(matches!(classify_health(&info), HealthVerdict::Failed(7)));
}

#[test]
fn health_exited_zero_is_satisfied() {
	// A one-shot that completed cleanly with no healthcheck is still satisfied.
	let info = inspect(r#"{"State":{"Status":"exited","ExitCode":0}}"#);
	assert!(matches!(
		classify_health(&info),
		HealthVerdict::NoHealthcheck
	));
}
