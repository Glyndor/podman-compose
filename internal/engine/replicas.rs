//! Replica-name resolution and ordering helpers.
//!
//! Pulled out of `engine::mod` so the engine module stays under the 500-line
//! hard cap enforced by the org's `line-limit` reusable. The two helpers are
//! small but tightly related: `resolve_replica_name` consults the running set
//! for the targeted replica, and `order_replicas` is the function that decides
//! what `--index None` means (the lowest-numbered one). Pure so both are
//! unit-testable without a Podman socket.

use crate::error::{ComposeError, Result};

/// Resolve a replica container name from the set of names that exist for a
/// service (the running replicas, or the statically derived names before
/// anything is created) and a 1-based `--index`. Each name is either the
/// unsuffixed base (the sole replica) or `{base}-{n}`.
///
/// `--index n` targets the replica numbered `n` — by name, not by position —
/// so it stays correct after a runtime `scale`/`up --scale` and regardless of
/// the order Podman lists containers; `0` is rejected (indexes are 1-based);
/// `None` picks the lowest-numbered replica. Pure so it is unit-testable
/// without a Podman socket.
pub(super) fn resolve_replica_name(
	service_name: &str,
	base: &str,
	names: &[String],
	index: Option<u32>,
) -> Result<String> {
	match index {
		Some(0) => Err(ComposeError::ReplicaIndex {
			service: service_name.to_string(),
			index: 0,
		}),
		Some(i) => {
			let suffixed = format!("{base}-{i}");
			if names.iter().any(|n| n == &suffixed) {
				return Ok(suffixed);
			}
			// A single, unsuffixed replica answers to index 1 only.
			if i == 1 && names.iter().any(|n| n == base) {
				return Ok(base.to_string());
			}
			Err(ComposeError::ReplicaIndex {
				service: service_name.to_string(),
				index: i,
			})
		}
		None => order_replicas(base, names)
			.into_iter()
			.next()
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into())),
	}
}

/// Order replica container names by their 1-based replica number so callers can
/// pick the lowest-numbered one independently of Podman's listing order. A name
/// is the unsuffixed base (the sole replica → number 1) or `{base}-{n}`; names
/// matching neither are dropped.
pub(super) fn order_replicas(base: &str, names: &[String]) -> Vec<String> {
	let prefix = format!("{base}-");
	let mut numbered: Vec<(usize, String)> = names
		.iter()
		.filter_map(|name| {
			if name == base {
				Some((1, name.clone()))
			} else {
				name.strip_prefix(&prefix)
					.and_then(|s| s.parse::<usize>().ok())
					.map(|n| (n, name.clone()))
			}
		})
		.collect();
	numbered.sort_by_key(|(n, _)| *n);
	numbered.into_iter().map(|(_, name)| name).collect()
}

#[cfg(test)]
mod tests {
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
}
