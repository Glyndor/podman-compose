//! Dependency ordering for `depends_on`: a topological sort of the service
//! graph.
//!
//! [`resolve_order`] gives a single start sequence; [`resolve_levels`] groups
//! services into levels that can start concurrently. Both reject a dependency
//! cycle and a reference to a missing service rather than starting in an
//! arbitrary order.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};

/// Compute a topological start order for all services (Kahn's algorithm).
///
/// Returns service names dependencies-first.
/// Errors on cycles ([`ComposeError::CircularDependency`]) or missing required
/// dependencies ([`ComposeError::ServiceNotFound`]).
pub fn resolve_order(file: &ComposeFile) -> Result<Vec<String>> {
	let services: Vec<&str> = file.services.keys().map(|s| s.as_str()).collect();
	let mut in_degree: HashMap<&str, usize> = services.iter().map(|&s| (s, 0)).collect();
	let mut graph: HashMap<&str, Vec<&str>> = services.iter().map(|&s| (s, vec![])).collect();

	for (name, service) in &file.services {
		for dep in service.depends_on.service_names() {
			if !file.services.contains_key(&dep) {
				if !service.depends_on.required_for(&dep) {
					continue;
				}
				return Err(ComposeError::ServiceNotFound(dep));
			}
			if let Some(neighbors) = graph.get_mut(dep.as_str()) {
				neighbors.push(name.as_str());
			}
			if let Some(deg) = in_degree.get_mut(name.as_str()) {
				*deg += 1;
			}
		}
	}

	// A min-heap (lexicographically smallest name first) keeps the order
	// deterministic: the in-degree map is a `HashMap`, so seeding/extending the
	// frontier from its iteration order would otherwise be per-run random. This
	// mirrors the per-level `sort_unstable` in `resolve_levels`, so independent
	// (in-degree-0) services resolve in a stable order — which `wait` relies on
	// for a reproducible exit code and output ordering.
	let mut queue: BinaryHeap<Reverse<&str>> = in_degree
		.iter()
		.filter(|(_, &deg)| deg == 0)
		.map(|(&s, _)| Reverse(s))
		.collect();

	let mut order = Vec::new();
	while let Some(Reverse(node)) = queue.pop() {
		order.push(node.to_string());
		let neighbors: Vec<&str> = graph.get(node).map_or(&[][..], |v| v.as_slice()).to_vec();
		for neighbor in neighbors {
			if let Some(deg) = in_degree.get_mut(neighbor) {
				*deg -= 1;
				if *deg == 0 {
					queue.push(Reverse(neighbor));
				}
			}
		}
	}

	if order.len() != services.len() {
		return Err(ComposeError::CircularDependency(cycle_message(&in_degree)));
	}

	Ok(order)
}

/// Build the user-facing circular-dependency message naming the services still
/// holding a nonzero in-degree after Kahn's algorithm — exactly the nodes that
/// form (or feed into) the cycle. Sorted for a deterministic message.
fn cycle_message(in_degree: &HashMap<&str, usize>) -> String {
	let mut involved: Vec<&str> = in_degree
		.iter()
		.filter(|(_, &deg)| deg > 0)
		.map(|(&s, _)| s)
		.collect();
	involved.sort_unstable();
	format!(
		"circular dependency among services: {}",
		involved.join(", ")
	)
}

/// Group services into dependency levels (Kahn's algorithm, layered).
///
/// Each returned level contains services whose dependencies all live in earlier
/// levels, so the services within one level have no `depends_on` relationship to
/// each other and can be started concurrently. Levels are ordered
/// dependencies-first; names within a level are sorted for deterministic
/// dispatch. Errors on cycles or missing required dependencies, matching
/// [`resolve_order`].
pub fn resolve_levels(file: &ComposeFile) -> Result<Vec<Vec<String>>> {
	let services: Vec<&str> = file.services.keys().map(|s| s.as_str()).collect();
	let mut in_degree: HashMap<&str, usize> = services.iter().map(|&s| (s, 0)).collect();
	let mut graph: HashMap<&str, Vec<&str>> = services.iter().map(|&s| (s, vec![])).collect();

	for (name, service) in &file.services {
		for dep in service.depends_on.service_names() {
			if !file.services.contains_key(&dep) {
				if !service.depends_on.required_for(&dep) {
					continue;
				}
				return Err(ComposeError::ServiceNotFound(dep));
			}
			if let Some(neighbors) = graph.get_mut(dep.as_str()) {
				neighbors.push(name.as_str());
			}
			if let Some(deg) = in_degree.get_mut(name.as_str()) {
				*deg += 1;
			}
		}
	}

	let mut current: Vec<&str> = in_degree
		.iter()
		.filter(|(_, &deg)| deg == 0)
		.map(|(&s, _)| s)
		.collect();

	let mut levels: Vec<Vec<String>> = Vec::new();
	let mut processed = 0;
	while !current.is_empty() {
		current.sort_unstable();
		let mut next: Vec<&str> = Vec::new();
		for &node in &current {
			processed += 1;
			let neighbors: Vec<&str> = graph.get(node).map_or(&[][..], |v| v.as_slice()).to_vec();
			for neighbor in neighbors {
				if let Some(deg) = in_degree.get_mut(neighbor) {
					*deg -= 1;
					if *deg == 0 {
						next.push(neighbor);
					}
				}
			}
		}
		levels.push(current.iter().map(|s| s.to_string()).collect());
		current = next;
	}

	if processed != services.len() {
		return Err(ComposeError::CircularDependency(cycle_message(&in_degree)));
	}

	Ok(levels)
}

#[cfg(test)]
#[path = "order_tests.rs"]
mod tests;
