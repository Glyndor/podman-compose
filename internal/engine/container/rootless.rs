//! Rootless-Podman caveat warnings: fields compose accepts that
//! rootless Podman on cgroups v2 cannot honour (or fails on). Pure helper
//! so it can be unit-tested without a live engine.

use crate::compose::types::Service;

/// Compose fields Podman accepts but cannot honor (or that fail) under rootless
/// Podman on cgroups v2. Returns advisory messages; pure so it can be
/// unit-tested. The wording mirrors podman-run(1) so operators are not misled
/// into assuming a no-op limit took effect.
pub(super) fn rootless_caveat_warnings(name: &str, service: &Service) -> Vec<String> {
	let mut out = Vec::new();
	if service.privileged == Some(true) {
		out.push(format!(
			"service \"{name}\": privileged has reduced effect under rootless Podman — a \
			container cannot gain more privileges than the user that launched it"
		));
	}
	if service.oom_kill_disable.is_some() {
		out.push(format!(
			"service \"{name}\": oom_kill_disable is not supported on cgroups v2 systems and \
			is ignored"
		));
	}
	if service.mem_swappiness.is_some() {
		out.push(format!(
			"service \"{name}\": mem_swappiness is only supported on cgroups v1 rootful systems \
			and is ignored otherwise"
		));
	}
	if service.cpu_rt_runtime.is_some() || service.cpu_rt_period.is_some() {
		out.push(format!(
			"service \"{name}\": cpu_rt_runtime/cpu_rt_period are only supported on cgroups v1 \
			rootful systems; the container may fail to start rootless"
		));
	}
	if !service.links.is_empty() {
		out.push(format!(
			"service \"{name}\": links has no effect under rootless Podman networking — put the \
			services on a shared network and reach them by service name instead"
		));
	}
	if !service.external_links.is_empty() {
		out.push(format!(
			"service \"{name}\": external_links has no effect under rootless Podman networking — \
			attach the target container to a shared network and reach it by service name instead"
		));
	}
	out
}

#[cfg(test)]
#[path = "rootless_tests.rs"]
mod tests;
