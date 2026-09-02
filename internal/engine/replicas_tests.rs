use super::{order_replicas, resolve_replica_name};
use crate::error::ComposeError;

fn names(list: &[&str]) -> Vec<String> {
	list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn resolve_replica_targets_running_scale_not_compose_default() {
	let live = names(&["proj-web-1", "proj-web-2", "proj-web-3"]);
	assert_eq!(
		resolve_replica_name("web", "proj-web", &live, Some(2)).unwrap(),
		"proj-web-2"
	);
	assert_eq!(
		resolve_replica_name("web", "proj-web", &live, Some(3)).unwrap(),
		"proj-web-3"
	);
}

#[test]
fn resolve_replica_is_order_independent() {
	let live = names(&["proj-web-3", "proj-web-1", "proj-web-2"]);
	assert_eq!(
		resolve_replica_name("web", "proj-web", &live, Some(1)).unwrap(),
		"proj-web-1"
	);
	assert_eq!(
		resolve_replica_name("web", "proj-web", &live, None).unwrap(),
		"proj-web-1"
	);
}

#[test]
fn resolve_replica_out_of_range_against_running_scale() {
	let live = names(&["proj-web-1", "proj-web-2"]);
	assert!(resolve_replica_name("web", "proj-web", &live, Some(3)).is_err());
}

#[test]
fn resolve_replica_index_zero_is_rejected() {
	let live = names(&["proj-web-1", "proj-web-2"]);
	let err = resolve_replica_name("web", "proj-web", &live, Some(0))
		.expect_err("index 0 must be rejected");
	assert!(
		matches!(err, ComposeError::ReplicaIndex { index: 0, ref service } if service == "web"),
		"unexpected error: {err:?}"
	);
}

#[test]
fn resolve_replica_single_unsuffixed_base() {
	let live = names(&["proj-web"]);
	assert_eq!(
		resolve_replica_name("web", "proj-web", &live, None).unwrap(),
		"proj-web"
	);
	assert_eq!(
		resolve_replica_name("web", "proj-web", &live, Some(1)).unwrap(),
		"proj-web"
	);
	assert!(resolve_replica_name("web", "proj-web", &live, Some(2)).is_err());
}

#[test]
fn order_replicas_sorts_by_replica_number() {
	let live = names(&["proj-web-10", "proj-web-2", "proj-web-1"]);
	assert_eq!(
		order_replicas("proj-web", &live),
		names(&["proj-web-1", "proj-web-2", "proj-web-10"])
	);
}
