use super::*;

#[test]
fn is_running_detects_live_statuses() {
	for up in ["running", "Up 2 minutes", "UP", "up about an hour"] {
		assert!(is_running(up), "{up} should be running");
	}
	for down in [
		"exited",
		"Exited (0) 3s ago",
		"created",
		"",
		"stopped",
		"paused",
	] {
		assert!(!is_running(down), "{down} should not be running");
	}
}

#[test]
fn split_ls_filters_buckets_and_flags_unknown() {
	let (names, status, unknown) = split_ls_filters(&[
		"name=web".to_string(),
		"status=RUNNING".to_string(),
		"bogus=1".to_string(),
	]);
	assert_eq!(names, vec!["web".to_string()]);
	assert_eq!(status, vec!["running".to_string()]);
	assert_eq!(unknown, vec!["bogus=1".to_string()]);
}

#[test]
fn ls_row_matches_applies_name_and_status() {
	// No filters → always matches.
	assert!(ls_row_matches("app", true, &[], &[]));
	// name substring.
	assert!(ls_row_matches("myapp", true, &["app".to_string()], &[]));
	assert!(!ls_row_matches("other", true, &["app".to_string()], &[]));
	// status word.
	assert!(ls_row_matches("app", true, &[], &["running".to_string()]));
	assert!(!ls_row_matches("app", false, &[], &["running".to_string()]));
	assert!(ls_row_matches("app", false, &[], &["exited".to_string()]));
}

#[test]
fn is_paused_detects_paused_statuses() {
	for p in ["paused", "Paused", "PAUSED"] {
		assert!(is_paused(p), "{p} should be paused");
	}
	for other in ["running", "exited", "created", ""] {
		assert!(!is_paused(other), "{other} should not be paused");
	}
}

#[test]
fn status_label_emits_per_state_counts() {
	// Mixed running + stopped keeps both counts instead of dropping the down one.
	assert_eq!(
		status_label(&Tally {
			running: 2,
			paused: 0,
			total: 3,
			..Default::default()
		}),
		"running(2), exited(1)"
	);
	// A paused project is labelled paused, not exited.
	assert_eq!(
		status_label(&Tally {
			running: 0,
			paused: 1,
			total: 1,
			..Default::default()
		}),
		"paused(1)"
	);
	// All up, all states present.
	assert_eq!(
		status_label(&Tally {
			running: 1,
			paused: 1,
			total: 3,
			..Default::default()
		}),
		"running(1), paused(1), exited(1)"
	);
	assert_eq!(
		status_label(&Tally {
			running: 0,
			paused: 0,
			total: 3,
			..Default::default()
		}),
		"exited(3)"
	);
}

/// #1082: `ConfigFiles` was hard-coded empty, present only for shape parity
/// with docker. It now carries the `podup.config-files` label the containers
/// were stamped with at creation.
#[test]
fn project_row_reports_the_recorded_config_files() {
	let tally = Tally {
		running: 1,
		total: 1,
		..Default::default()
	};
	let row = project_row("web", &tally, "/srv/app/compose.yaml");
	assert_eq!(row["Name"], "web");
	assert_eq!(row["Status"], "running(1)");
	assert_eq!(row["ConfigFiles"], "/srv/app/compose.yaml");
}

/// A container created before the label existed, or by an embedder that
/// supplied no paths, reports empty rather than failing.
#[test]
fn project_row_tolerates_an_unrecorded_config_file() {
	let tally = Tally {
		running: 1,
		total: 1,
		..Default::default()
	};
	assert_eq!(project_row("web", &tally, "")["ConfigFiles"], "");
}
