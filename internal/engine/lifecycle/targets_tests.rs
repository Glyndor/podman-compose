use super::{
	expand_targets, filter_services, in_started_set, service_grace_period_secs, stop_deadline,
	stop_timeout_param, validate_stop_timeout, validate_targets, STOP_GRACE_BUFFER_SECS,
};
use crate::compose::types::{ComposeFile, Service};
use crate::error::ComposeError;
use std::collections::HashSet;
use std::time::Duration;

// --- validate_stop_timeout (#778) ---

#[test]
fn validate_stop_timeout_accepts_none_zero_and_positive() {
	assert_eq!(validate_stop_timeout(None).unwrap(), None);
	assert_eq!(validate_stop_timeout(Some(0)).unwrap(), Some(0));
	assert_eq!(validate_stop_timeout(Some(30)).unwrap(), Some(30));
}

#[test]
fn validate_stop_timeout_accepts_minus_one_infinite() {
	// -1 is docker's "wait indefinitely" sentinel and must pass through.
	assert_eq!(validate_stop_timeout(Some(-1)).unwrap(), Some(-1));
}

#[test]
fn validate_stop_timeout_rejects_below_minus_one() {
	// A value below -1 is rejected here rather than leaking a raw libpod 400.
	let err = validate_stop_timeout(Some(-2)).unwrap_err();
	assert!(matches!(err, ComposeError::InvalidTimeout(-2)));
	assert!(validate_stop_timeout(Some(-100)).is_err());
}

// --- stop_timeout_param (#778) ---

#[test]
fn stop_timeout_param_passes_through_non_negative() {
	assert_eq!(stop_timeout_param(0), 0);
	assert_eq!(stop_timeout_param(10), 10);
}

#[test]
fn stop_timeout_param_maps_infinite_to_max() {
	// -1 (infinite) maps to the largest value libpod accepts so podman never
	// escalates to SIGKILL on its own, matching `docker stop -t -1`.
	assert_eq!(stop_timeout_param(-1), i64::from(i32::MAX));
}

// --- stop_deadline (#719) ---

#[test]
fn stop_deadline_is_grace_plus_buffer() {
	assert_eq!(
		stop_deadline(10),
		Some(Duration::from_secs(10 + STOP_GRACE_BUFFER_SECS))
	);
	assert_eq!(
		stop_deadline(0),
		Some(Duration::from_secs(STOP_GRACE_BUFFER_SECS))
	);
}

#[test]
fn stop_deadline_infinite_is_none() {
	// -1 leaves the stop uncapped (docker `stop -t -1` parity).
	assert_eq!(stop_deadline(-1), None);
}

// --- service_grace_period_secs ---

#[test]
fn grace_period_defaults_to_ten_seconds() {
	// No stop_grace_period set → the docker-compose default of 10s.
	assert_eq!(service_grace_period_secs(&Service::default()), 10);
}

#[test]
fn grace_period_parses_duration() {
	// Plain seconds and a single-unit minutes value both resolve.
	let svc = Service {
		stop_grace_period: Some("90s".to_string()),
		..Default::default()
	};
	assert_eq!(service_grace_period_secs(&svc), 90);

	let svc = Service {
		stop_grace_period: Some("2m".to_string()),
		..Default::default()
	};
	assert_eq!(service_grace_period_secs(&svc), 120);
}

#[test]
fn grace_period_falls_back_on_unparseable() {
	// A value that does not parse as a duration falls back to the default.
	let svc = Service {
		stop_grace_period: Some("not-a-duration".to_string()),
		..Default::default()
	};
	assert_eq!(service_grace_period_secs(&svc), 10);
}

fn file_with_services(names: &[&str]) -> ComposeFile {
	let mut file = ComposeFile::default();
	for &name in names {
		file.services.insert(name.to_string(), Service::default());
	}
	file
}

#[test]
fn filter_empty_target_returns_all() {
	let file = file_with_services(&["a", "b", "c"]);
	let order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
	let result = filter_services(&file, order.clone(), &[]).unwrap();
	assert_eq!(result, order);
}

#[test]
fn filter_target_subset_returns_intersection() {
	let file = file_with_services(&["a", "b", "c"]);
	let order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
	let result = filter_services(&file, order, &["b".to_string()]).unwrap();
	assert_eq!(result, vec!["b".to_string()]);
}

#[test]
fn filter_target_preserves_order() {
	let file = file_with_services(&["a", "b", "c"]);
	let order = vec!["a".to_string(), "b".to_string(), "c".to_string()];
	let result = filter_services(&file, order, &["c".to_string(), "a".to_string()]).unwrap();
	assert_eq!(result, vec!["a".to_string(), "c".to_string()]);
}

#[test]
fn filter_unknown_service_returns_error() {
	let file = file_with_services(&["a"]);
	let order = vec!["a".to_string()];
	let err = filter_services(&file, order, &["z".to_string()]).unwrap_err();
	assert!(matches!(
		err,
		crate::error::ComposeError::ServiceNotFound(_)
	));
}

// --- validate_targets ---

#[test]
fn validate_targets_empty_is_ok() {
	let file = file_with_services(&["a", "b"]);
	assert!(validate_targets(&file, &[]).is_ok());
}

#[test]
fn validate_targets_known_names_ok() {
	let file = file_with_services(&["a", "b"]);
	assert!(validate_targets(&file, &["a".to_string(), "b".to_string()]).is_ok());
}

#[test]
fn validate_targets_unknown_name_errors() {
	// An `up`/`create` for a service the file does not define must error
	// rather than silently match nothing and exit 0.
	let file = file_with_services(&["a"]);
	let err = validate_targets(&file, &["no-such-service".to_string()]).unwrap_err();
	assert!(matches!(
		err,
		crate::error::ComposeError::ServiceNotFound(name) if name == "no-such-service"
	));
}

// --- expand_targets ---

fn file_web_depends_db() -> ComposeFile {
	crate::parse_str(
		"services:\n  db:\n    image: x\n  web:\n    image: x\n    depends_on:\n      - db\n",
	)
	.unwrap()
}

#[test]
fn expand_targets_empty_is_none() {
	let file = file_web_depends_db();
	assert!(expand_targets(&file, &[], false).is_none());
}

#[test]
fn expand_targets_includes_dependencies() {
	let file = file_web_depends_db();
	let set = expand_targets(&file, &["web".to_string()], false).unwrap();
	assert!(set.contains("web"));
	assert!(set.contains("db"));
}

#[test]
fn expand_targets_no_deps_excludes_dependencies() {
	let file = file_web_depends_db();
	let set = expand_targets(&file, &["web".to_string()], true).unwrap();
	assert!(set.contains("web"));
	assert!(!set.contains("db"));
}

// --- in_started_set ---

#[test]
fn in_started_set_none_is_always_true() {
	// No explicit target list: every service (including any dependency)
	// is in scope, so the readiness wait is never skipped.
	assert!(in_started_set(&None, "anything"));
}

#[test]
fn in_started_set_member_is_true() {
	let set: HashSet<String> = ["web".to_string(), "db".to_string()].into_iter().collect();
	assert!(in_started_set(&Some(set), "db"));
}

#[test]
fn in_started_set_excluded_dep_is_false() {
	// Mirrors `up web --no-deps`: `expand_targets` yields {web} only, so the
	// excluded dependency `db` is not in the started set and its readiness
	// wait must be skipped.
	let file = file_web_depends_db();
	let target_set = expand_targets(&file, &["web".to_string()], true);
	assert!(in_started_set(&target_set, "web"));
	assert!(!in_started_set(&target_set, "db"));
}

#[test]
fn in_started_set_partial_target_includes_transitive_dep() {
	// `up web` (without --no-deps) pulls `db` into the set, so its readiness
	// wait is still honored.
	let file = file_web_depends_db();
	let target_set = expand_targets(&file, &["web".to_string()], false);
	assert!(in_started_set(&target_set, "web"));
	assert!(in_started_set(&target_set, "db"));
}
