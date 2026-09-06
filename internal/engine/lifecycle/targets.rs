//! Service-selection helpers for lifecycle commands: stop grace period,
//! target-list filtering, and `depends_on` expansion.

use std::collections::HashSet;
use std::time::Duration;

use crate::compose::types::{ComposeFile, Service};
use crate::error::{ComposeError, Result};

/// Extra wall-clock slack, beyond the grace period, before podup gives up on a
/// stalled libpod `stop` and escalates to a client-side `SIGKILL`. Podman stops
/// a container by sending `SIGTERM`, waiting the grace window, then `SIGKILL`
/// itself, so a healthy stop returns at most ~`grace` seconds in. The buffer
/// absorbs daemon/reap latency so a slow-but-working stop is never escalated;
/// anything past it means the libpod call is wedged and we kill independently.
const STOP_GRACE_BUFFER_SECS: u64 = 30;

/// The per-service shutdown grace from `stop_grace_period` (default 10s).
pub(super) fn service_grace_period_secs(service: &Service) -> i32 {
	service
		.stop_grace_period
		.as_deref()
		.and_then(crate::size::parse_duration_secs)
		.and_then(|s| i32::try_from(s).ok())
		.unwrap_or(10)
}

/// Validate a CLI `-t/--timeout` value at the trust boundary.
///
/// `-1` (docker's "wait indefinitely") and any non-negative second count are
/// accepted; anything below `-1` is rejected with [`ComposeError::InvalidTimeout`]
/// so it never reaches libpod as a `?t=<negative>` that surfaces a raw `HTTP 400`.
/// Pure so the boundary check is unit-tested without a live socket.
pub fn validate_stop_timeout(timeout: Option<i32>) -> Result<Option<i32>> {
	match timeout {
		Some(t) if t < -1 => Err(ComposeError::InvalidTimeout(t)),
		other => Ok(other),
	}
}

/// The libpod `?t=` value for a grace period. A non-negative grace passes through;
/// `-1` ("wait indefinitely") maps to the largest value libpod accepts so podman
/// does not escalate to `SIGKILL` on its own, matching `docker stop -t -1`. Pure.
pub(super) fn stop_timeout_param(grace: i32) -> i64 {
	if grace < 0 {
		i64::from(i32::MAX)
	} else {
		i64::from(grace)
	}
}

/// Client-side deadline for a `stop` call: the grace window plus
/// [`STOP_GRACE_BUFFER_SECS`]. `-1` ("wait indefinitely") yields `None`, leaving
/// the call uncapped like `docker stop -t -1`. Pure so the policy is unit-tested.
pub(super) fn stop_deadline(grace: i32) -> Option<Duration> {
	if grace < 0 {
		None
	} else {
		Some(Duration::from_secs(grace as u64 + STOP_GRACE_BUFFER_SECS))
	}
}

impl crate::engine::Engine {
	/// Shutdown grace (seconds) for a service: the CLI `-t/--timeout` override
	/// when set, otherwise the service's `stop_grace_period`.
	pub(super) fn grace_period_secs(&self, service: &Service) -> i32 {
		self.stop_timeout
			.unwrap_or_else(|| service_grace_period_secs(service))
	}
}

/// Return the ordered service names filtered to `target_services`.
///
/// Returns an error if any name in `target_services` is not in the file.
pub(super) fn filter_services(
	file: &ComposeFile,
	order: Vec<String>,
	target_services: &[String],
) -> Result<Vec<String>> {
	if target_services.is_empty() {
		return Ok(order);
	}
	for name in target_services {
		if !file.services.contains_key(name) {
			return Err(ComposeError::ServiceNotFound(name.clone()));
		}
	}
	let set: std::collections::HashSet<&str> = target_services.iter().map(|s| s.as_str()).collect();
	Ok(order
		.into_iter()
		.filter(|n| set.contains(n.as_str()))
		.collect())
}

/// Error if any requested target service name is absent from the file.
///
/// The up/create path expands targets into a set without checking membership, so
/// a bogus name would silently match nothing and exit 0. This validates the list
/// up front, matching docker-compose and the stop/start/kill commands, which
/// already reject unknown services via [`filter_services`].
pub(super) fn validate_targets(file: &ComposeFile, target_services: &[String]) -> Result<()> {
	for name in target_services {
		if !file.services.contains_key(name) {
			return Err(ComposeError::ServiceNotFound(name.clone()));
		}
	}
	Ok(())
}

/// Resolve which services `up`/`create` should start given an explicit target
/// list.
///
/// Returns `None` when no targets are given (start everything). Otherwise the set
/// contains the targets plus, unless `no_deps` is set, their transitive
/// `depends_on` services. Callers must validate `target_services` up front via
/// [`validate_targets`] so a typo'd/unknown name fails loudly instead of being
/// silently skipped, mirroring [`filter_services`] (and docker compose's "no
/// such service").
pub(super) fn expand_targets(
	file: &ComposeFile,
	target_services: &[String],
	no_deps: bool,
) -> Option<HashSet<String>> {
	if target_services.is_empty() {
		return None;
	}
	let mut set = HashSet::new();
	let mut stack: Vec<String> = target_services.to_vec();
	while let Some(name) = stack.pop() {
		if !set.insert(name.clone()) {
			continue;
		}
		if !no_deps {
			if let Some(service) = file.services.get(&name) {
				for dep in service.depends_on.service_names() {
					if !set.contains(&dep) {
						stack.push(dep);
					}
				}
			}
		}
	}
	Some(set)
}

/// Whether `name` is part of the started set described by `target_set`.
///
/// `target_set` is `None` when no explicit target list was given (every
/// service is in scope, so the answer is always `true`). Otherwise a name is
/// "started" only if it is present in the set. Under `up --no-deps`,
/// [`expand_targets`] omits the targets' dependencies, so this returns `false`
/// for an intentionally-excluded dependency, letting the caller skip its
/// `depends_on` readiness wait, matching docker-compose.
pub(super) fn in_started_set(target_set: &Option<HashSet<String>>, name: &str) -> bool {
	match target_set {
		None => true,
		Some(set) => set.contains(name),
	}
}

#[cfg(test)]
#[path = "targets_tests.rs"]
mod tests;
