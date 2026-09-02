use super::*;
use crate::compose::types::Service;
use crate::error::ComposeError;

fn levels(input: &[&[&str]]) -> Vec<Vec<String>> {
	input
		.iter()
		.map(|lvl| lvl.iter().map(|s| s.to_string()).collect())
		.collect()
}

fn file_with(services: &[&str]) -> ComposeFile {
	let mut file = ComposeFile::default();
	for &s in services {
		file.services.insert(s.to_string(), Service::default());
	}
	file
}

#[tokio::test]
async fn join_bounded_preserves_input_order() {
	// Futures finish out of order (later index resolves first) but the
	// collected results are returned in input order.
	let futs = (0..5usize).map(|i| async move {
		// Smaller i yields later via more yields, so completion order != input.
		for _ in 0..(5 - i) {
			tokio::task::yield_now().await;
		}
		if i == 2 {
			Err(ComposeError::ServiceNotFound(format!("svc{i}")))
		} else {
			Ok(())
		}
	});
	let results = join_bounded(futs).await;
	assert_eq!(results.len(), 5);
	// Only index 2 is an error, proving order is preserved.
	for (i, r) in results.iter().enumerate() {
		assert_eq!(r.is_err(), i == 2, "index {i}");
	}
}

#[test]
fn first_error_returns_first_in_order() {
	let results = vec![
		Ok(()),
		Err(ComposeError::ServiceNotFound("a".into())),
		Err(ComposeError::ServiceNotFound("b".into())),
	];
	let err = first_error(results).unwrap();
	assert!(matches!(err, ComposeError::ServiceNotFound(n) if n == "a"));
}

#[test]
fn first_error_none_when_all_ok() {
	assert!(first_error(vec![Ok(()), Ok(())]).is_none());
}

#[test]
fn filter_levels_empty_targets_keeps_all() {
	let file = file_with(&["a", "b", "c"]);
	let lv = levels(&[&["a", "b"], &["c"]]);
	let out = filter_levels(&file, lv.clone(), &[]).unwrap();
	assert_eq!(out, lv);
}

#[test]
fn filter_levels_drops_empty_levels() {
	let file = file_with(&["a", "b", "c"]);
	let lv = levels(&[&["a", "b"], &["c"]]);
	let out = filter_levels(&file, lv, &["c".into()]).unwrap();
	assert_eq!(out, levels(&[&["c"]]));
}

#[test]
fn filter_levels_unknown_target_errors() {
	let file = file_with(&["a"]);
	let lv = levels(&[&["a"]]);
	let err = filter_levels(&file, lv, &["z".into()]).unwrap_err();
	assert!(matches!(err, ComposeError::ServiceNotFound(n) if n == "z"));
}

#[test]
fn retain_levels_filters_and_drops() {
	let lv = levels(&[&["a", "b"], &["c"]]);
	let keep: HashSet<&str> = ["a", "c"].into_iter().collect();
	let out = retain_levels(lv, |n| keep.contains(n));
	assert_eq!(out, levels(&[&["a"], &["c"]]));
}

#[test]
fn restart_service_set_empty_targets_is_all() {
	let file = file_with(&["a", "b"]);
	let (full, targets) = restart_service_set(&file, &[], false);
	assert_eq!(full, targets);
	assert_eq!(full.len(), 2);
	assert!(full.contains("a") && full.contains("b"));
}

#[test]
fn restart_service_set_includes_cascade_dependents() {
	// web depends_on db with restart: true → restarting db cascades to web.
	let file = crate::parse_str(
		"services:\n  db:\n    image: x\n  web:\n    image: x\n    depends_on:\n      db:\n        condition: service_started\n        restart: true\n",
	)
	.unwrap();
	let (full, targets) = restart_service_set(&file, &["db".into()], false);
	assert!(targets.contains("db") && targets.len() == 1);
	assert!(full.contains("db") && full.contains("web"));
}

#[test]
fn restart_service_set_no_deps_excludes_cascade() {
	let file = crate::parse_str(
		"services:\n  db:\n    image: x\n  web:\n    image: x\n    depends_on:\n      db:\n        condition: service_started\n        restart: true\n",
	)
	.unwrap();
	let (full, _) = restart_service_set(&file, &["db".into()], true);
	assert!(full.contains("db"));
	assert!(!full.contains("web"));
}
