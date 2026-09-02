//! Report compose fields that are set but have no Quadlet mapping.

use indexmap::IndexMap;

use crate::compose::types::{DependsOn, HealthCheck, Service, ServiceCondition};

/// Warn for fields that are set but have no Quadlet mapping, so the operator
/// knows the generated unit is incomplete rather than discovering it at run
/// time.
pub(super) fn collect_warnings(
	name: &str,
	service: &Service,
	services: &IndexMap<String, Service>,
	warnings: &mut Vec<String>,
) {
	let mut warn = |field: &str, detail: &str| {
		warnings.push(format!("{name}: {field} {detail}"));
	};
	let replicas = service
		.scale
		.or(service.deploy.as_ref().and_then(|d| d.replicas));
	if replicas.is_some_and(|r| r > 1) {
		warn(
			"scale/replicas",
			"is ignored; Quadlet emits a single container per service",
		);
	}
	if !service.configs.is_empty() {
		warn("configs", "have no Quadlet equivalent and are skipped");
	}
	if !service.volumes_from.is_empty() {
		warn("volumes_from", "has no Quadlet equivalent and is skipped");
	}
	// `host`/`none` map to `Network=`, and `service:X`/`container:X` map to
	// `Network=X.container`; only the remaining modes (bridge:, custom, …) have
	// no key.
	if service.network_mode.as_deref().is_some_and(|m| {
		m != "host" && m != "none" && !m.starts_with("service:") && !m.starts_with("container:")
	}) {
		warn(
			"network_mode",
			"is not mapped (only `host`/`none`/`service:`/`container:` are supported); use networks instead",
		);
	}
	if !service.profiles.is_empty() {
		warn("profiles", "have no Quadlet equivalent and are ignored");
	}
	if !service.post_start.is_empty() {
		warn(
			"post_start",
			"hooks have no Quadlet equivalent and are skipped",
		);
	}
	if !service.pre_stop.is_empty() {
		warn(
			"pre_stop",
			"hooks have no Quadlet equivalent and are skipped",
		);
	}
	// `service_healthy` IS enforceable, as long as the service it names has a
	// healthcheck: that service's unit carries `Notify=healthy`, so systemd
	// does not call it started until the probe passes, and `After=`/`Requires=`
	// then order against readiness rather than creation. Measured on podman
	// 5.7.0 — a dependant started 10s into a dependency whose probe took 10s.
	//
	// So the warning is now about the cases that really cannot work: a
	// `service_healthy` naming a service with no healthcheck to wait on, and
	// `service_completed_successfully`, which would need the dependency to be a
	// `Type=oneshot` unit that exits.
	if let DependsOn::Map(deps) = &service.depends_on {
		let unwaitable: Vec<&str> = deps
			.iter()
			.filter(|(dep_name, c)| match c.condition {
				ServiceCondition::ServiceStarted => false,
				ServiceCondition::ServiceHealthy => services
					.get(dep_name.as_str())
					.is_none_or(|d| d.healthcheck.as_ref().is_none_or(HealthCheck::is_disabled)),
				_ => true,
			})
			.map(|(dep_name, _)| dep_name.as_str())
			.collect();
		if !unwaitable.is_empty() {
			warn(
				"depends_on",
				&format!(
					"condition on {} is not enforceable in Quadlet and only start ordering is emitted; service_healthy needs the named service to declare a healthcheck, and service_completed_successfully has no equivalent",
					unwaitable.join(", ")
				),
			);
		}
	}

	// `env_file: [{path, required: false}]` means a missing file is not an error.
	// Quadlet has no way to say that: `EnvironmentFile=` becomes
	// `podman run --env-file`, which is fatal on a missing path. So an entry the
	// compose file marks optional becomes a container that refuses to start —
	// and this is the deployment-local override pattern (a `.env.production`
	// deliberately absent from the repo), so it fails exactly where the file is
	// meant to be missing.
	if service.env_file.to_entries().iter().any(|e| !e.required()) {
		warn(
			"env_file",
			"`required: false` cannot be expressed in Quadlet; the file will be required at run time and the container will not start without it",
		);
	}

	// Fields that are honoured at runtime but have no [Container] Quadlet key and
	// no unambiguous PodmanArgs= fallback. Warn so the generated unit is never
	// silently incomplete; add the flag by hand if it is required.
	let skipped = "has no Quadlet equivalent and is skipped";
	if service.ipc.is_some() {
		warn("ipc", skipped);
	}
	if service.pid.is_some() {
		warn("pid", skipped);
	}
	if service.uts.is_some() {
		warn("uts", skipped);
	}
	if service.cgroup.is_some() {
		warn("cgroup", skipped);
	}
	if service.cgroup_parent.is_some() {
		warn("cgroup_parent", skipped);
	}
	if service.runtime.is_some() {
		warn("runtime", skipped);
	}
	if service.tty.is_some() {
		warn("tty", skipped);
	}
	if service.stdin_open.is_some() {
		warn("stdin_open", skipped);
	}
	if service.memswap_limit.is_some() {
		warn("memswap_limit", skipped);
	}
	if service.mem_reservation.is_some() {
		warn("mem_reservation", skipped);
	}
	if service.oom_kill_disable.is_some() {
		warn("oom_kill_disable", skipped);
	}
	if service.oom_score_adj.is_some() {
		warn("oom_score_adj", skipped);
	}
	if service.blkio_config.is_some() {
		warn("blkio_config", skipped);
	}
	if service.gpus.is_some() {
		warn(
			"gpus",
			"has no Quadlet equivalent and is skipped; GPU devices are not assigned",
		);
	}
	if service.platform.is_some() {
		warn("platform", skipped);
	}
	if !service.device_cgroup_rules.is_empty() {
		warn("device_cgroup_rules", skipped);
	}
	if !service.storage_opt.is_empty() {
		warn("storage_opt", skipped);
	}
	if !service.links.is_empty() {
		warn("links", skipped);
	}
	if !service.external_links.is_empty() {
		warn("external_links", skipped);
	}
	if service.domainname.is_some() {
		warn("domainname", skipped);
	}
	if service.mem_swappiness.is_some() {
		warn("mem_swappiness", skipped);
	}
	if service.cpu_rt_runtime.is_some() {
		warn("cpu_rt_runtime", skipped);
	}
	if service.cpu_rt_period.is_some() {
		warn("cpu_rt_period", skipped);
	}
	if service.cpu_count.is_some() {
		warn("cpu_count", skipped);
	}
	if service.cpu_percent.is_some() {
		warn("cpu_percent", skipped);
	}
	if service.attach.is_some() {
		warn("attach", skipped);
	}
	if service.develop.is_some() {
		warn("develop", skipped);
	}
	if service.credential_spec.is_some() {
		warn("credential_spec", skipped);
	}
	if service.isolation.is_some() {
		warn("isolation", skipped);
	}
	if service.provider.is_some() {
		warn("provider", skipped);
	}
	if service.use_api_socket.is_some() {
		warn("use_api_socket", skipped);
	}
	if !service.label_file.to_list().is_empty() {
		warn("label_file", skipped);
	}
	// MAC addresses have no Quadlet key (service-level or per-network), and a
	// per-network value cannot be expressed via the whole-container PodmanArgs=.
	let has_network_mac = service
		.networks
		.names()
		.iter()
		.filter_map(|n| service.networks.config_for(n))
		.any(|c| c.mac_address.is_some());
	if service.mac_address.is_some() || has_network_mac {
		warn("mac_address", skipped);
	}
	// Only the first static IP across the service's networks is emitted; a second
	// one would need per-network IP scoping that Quadlet does not support.
	let static_ip_count = service
		.networks
		.names()
		.iter()
		.filter_map(|n| service.networks.config_for(n))
		.filter(|c| c.ipv4_address.is_some() || c.ipv6_address.is_some())
		.count();
	if static_ip_count > 1 {
		warn(
			"ipv4_address/ipv6_address",
			"is set on multiple networks; Quadlet emits only the first (no per-network IP scoping)",
		);
	}
}

#[cfg(test)]
#[path = "warnings_tests.rs"]
mod tests;
