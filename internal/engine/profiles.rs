//! Profile filtering — determines which services run given the active profile set.

use std::collections::HashSet;

use crate::compose::types::{ComposeFile, Service};

/// Remove services excluded by the active profile set, in place.
///
/// `active` is the CLI `--profile` list, falling back to `COMPOSE_PROFILES`.
/// A profiled service that is a transitive `depends_on` target of a retained
/// service is implicitly enabled, matching docker compose — so the output never
/// carries a dangling dependency reference.
pub fn retain_active_profiles(file: &mut ComposeFile, active: &[String]) {
	retain_active_profiles_with_targets(file, active, &[]);
}

/// Like [`retain_active_profiles`], but also keeps any service named in
/// `targets` even when its profile is inactive: naming a service on the command
/// line activates its profile (docker compose), so per-service subcommands
/// (`start`, `stop`, `build`, `push`, `pull`, …) can still address it.
pub fn retain_active_profiles_with_targets(
	file: &mut ComposeFile,
	active: &[String],
	targets: &[String],
) {
	let set = active_profiles_set(active);
	let enabled = enabled_profile_services(file, &set, targets);
	file.services.retain(|name, _| enabled.contains(name));
}

/// The set of service names that should run under the active profile set.
///
/// A service is enabled when it is unprofiled, matches an active profile (or the
/// `*` wildcard), or is explicitly named in `targets` (naming a service on the
/// command line activates its profile). Implicit activation then pulls in the
/// transitive `depends_on` targets of every enabled service — even profiled
/// ones whose profile is inactive — so a started service never depends on a
/// service that was filtered out. Mirrors docker compose, which activates a
/// profiled service that is depended on by a started one.
///
/// This is the single source of truth for "which services does an `up`/`config`
/// with these profiles touch": [`retain_active_profiles_with_targets`] uses it
/// to prune the config, and the `up`/`create` lifecycle path uses it to decide
/// which services to actually start — so the two never disagree.
pub(crate) fn enabled_profile_services(
	file: &ComposeFile,
	active: &HashSet<String>,
	targets: &[String],
) -> HashSet<String> {
	let named: HashSet<&str> = targets.iter().map(|s| s.as_str()).collect();

	// Directly enabled: an unprofiled service, a profile match (or `*`), or a
	// service explicitly named on the command line.
	let mut enabled: HashSet<String> = file
		.services
		.iter()
		.filter(|(name, svc)| service_in_profiles(svc, active) || named.contains(name.as_str()))
		.map(|(name, _)| name.clone())
		.collect();

	// Implicit activation: pull in profiled `depends_on` targets of enabled
	// services, transitively, so a retained service never references a dropped
	// dependency.
	let mut stack: Vec<String> = enabled.iter().cloned().collect();
	while let Some(name) = stack.pop() {
		if let Some(svc) = file.services.get(&name) {
			for dep in svc.depends_on.service_names() {
				if file.services.contains_key(&dep) && enabled.insert(dep.clone()) {
					stack.push(dep);
				}
			}
		}
	}

	enabled
}

/// Build the active-profile set, falling back to `COMPOSE_PROFILES` env var.
pub(super) fn active_profiles_set(active: &[String]) -> HashSet<String> {
	if !active.is_empty() {
		return active.iter().cloned().collect();
	}
	std::env::var("COMPOSE_PROFILES")
		.ok()
		.map(|s| {
			s.split(',')
				.map(|p| p.trim().to_string())
				.filter(|p| !p.is_empty())
				.collect()
		})
		.unwrap_or_default()
}

/// True if the service should be started given the active profile set.
///
/// Services with no profiles always start. A literal `*` in the active set is a
/// wildcard that enables every profiled service (docker compose's
/// "enable all profiles").
pub(super) fn service_in_profiles(service: &Service, active: &HashSet<String>) -> bool {
	if service.profiles.is_empty() {
		return true;
	}
	if active.contains("*") {
		return true;
	}
	service.profiles.iter().any(|p| active.contains(p))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "profiles_tests.rs"]
mod tests;
