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
mod tests {
	use crate::parse_str;
	use crate::quadlet::generate_at;

	#[test]
	fn warns_for_every_unmapped_field() {
		let yaml = r#"
services:
  everything:
    image: app:1.0
    build: .
    scale: 3
    network_mode: "bridge:custom"
    volumes_from:
      - other
    profiles:
      - debug
    healthcheck:
      test: ["CMD", "true"]
    secrets:
      - my_secret
    configs:
      - my_config
secrets:
  my_secret:
    file: ./s.txt
configs:
  my_config:
    file: ./c.txt
"#;
		let file = parse_str(yaml).unwrap();
		let warnings = generate_at(&file, "proj", std::path::Path::new("/srv/app")).warnings;
		let joined = warnings.join("\n");

		for field in [
			"scale/replicas",
			"configs",
			"volumes_from",
			"network_mode",
			"profiles",
		] {
			assert!(
				joined.contains(field),
				"missing warning for {field}; got:\n{joined}"
			);
		}
		// secrets are now mapped to Secret=, so they must NOT warn.
		assert!(
			!joined.contains("secrets"),
			"secrets should be mapped, not warned; got:\n{joined}"
		);
		// privileged is now mapped to PodmanArgs=--privileged, not warned.
		assert!(
			!joined.contains("privileged"),
			"privileged should be mapped, not warned; got:\n{joined}"
		);
	}

	#[test]
	fn service_and_container_network_modes_are_mapped_not_warned() {
		// `service:X`/`container:X` map to `Network=X.container`, so they must not
		// warn; only other unmapped modes (bridge:, custom, …) warn.
		for mode in ["service:db", "container:other"] {
			let yaml = format!("services:\n  s:\n    image: x\n    network_mode: \"{mode}\"\n");
			let file = parse_str(&yaml).unwrap();
			let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
				.warnings
				.join("\n");
			assert!(
				!joined.contains("network_mode"),
				"{mode} should be mapped, not warned; got:\n{joined}"
			);
		}
	}

	#[test]
	fn warns_for_silently_dropped_runtime_fields() {
		let yaml = r#"
services:
  s:
    image: x
    ipc: host
    pid: host
    uts: host
    cgroup: private
    cgroup_parent: /sys/fs/cgroup/p
    runtime: crun
    tty: true
    stdin_open: true
    mac_address: "02:42:ac:11:00:02"
    memswap_limit: 1g
    mem_reservation: 256m
    oom_kill_disable: true
    oom_score_adj: -500
    label_file:
      - ./labels.env
    blkio_config:
      weight: 300
"#;
		let file = parse_str(yaml).unwrap();
		let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
			.warnings
			.join("\n");
		for field in [
			"ipc",
			"pid",
			"uts",
			"cgroup",
			"cgroup_parent",
			"runtime",
			"tty",
			"stdin_open",
			"mac_address",
			"memswap_limit",
			"mem_reservation",
			"oom_kill_disable",
			"oom_score_adj",
			"label_file",
			"blkio_config",
		] {
			assert!(
				joined.contains(field),
				"missing warning for {field}; got:\n{joined}"
			);
		}
	}

	#[test]
	fn warns_for_additional_silently_dropped_fields() {
		let yaml = r#"
services:
  s:
    image: x
    gpus: all
    platform: linux/arm64
    domainname: example.internal
    links:
      - other
    external_links:
      - ext:alias
    device_cgroup_rules:
      - "c 1:3 rwm"
    storage_opt:
      size: 10G
    mem_swappiness: 50
    cpu_rt_runtime: 95000
    cpu_rt_period: 1000000
"#;
		let file = parse_str(yaml).unwrap();
		let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
			.warnings
			.join("\n");
		for field in [
			"gpus",
			"platform",
			"domainname",
			"links",
			"external_links",
			"device_cgroup_rules",
			"storage_opt",
			"mem_swappiness",
			"cpu_rt_runtime",
			"cpu_rt_period",
		] {
			assert!(
				joined.contains(field),
				"missing warning for {field}; got:\n{joined}"
			);
		}
	}

	#[test]
	fn security_opt_mask_and_unmask_map_to_keys_not_warned() {
		let yaml = r#"
services:
  s:
    image: x
    security_opt:
      - "mask=/proc/kcore:/proc/timer_list"
      - "unmask=ALL"
"#;
		let file = parse_str(yaml).unwrap();
		let out = generate_at(&file, "proj", std::path::Path::new("/srv/app"));
		let contents = out
			.units
			.iter()
			.map(|u| u.contents.as_str())
			.collect::<String>();
		assert!(
			contents.contains("Mask=/proc/kcore:/proc/timer_list"),
			"missing Mask= key; got:\n{contents}"
		);
		assert!(
			contents.contains("Unmask=ALL"),
			"missing Unmask= key; got:\n{contents}"
		);
		assert!(
			!out.warnings.iter().any(|w| w.contains("security_opt")),
			"mask/unmask should be mapped, not warned; got:\n{:?}",
			out.warnings
		);
	}

	#[test]
	fn warns_when_static_ip_set_on_multiple_networks() {
		let yaml = r#"
services:
  s:
    image: x
    networks:
      a:
        ipv4_address: 10.0.0.2
      b:
        ipv4_address: 10.0.1.2
networks:
  a:
  b:
"#;
		let file = parse_str(yaml).unwrap();
		let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
			.warnings
			.join("\n");
		assert!(
			joined.contains("ipv4_address/ipv6_address"),
			"missing multi-network static-IP warning; got:\n{joined}"
		);
	}

	#[test]
	fn warns_for_parsed_but_unmapped_service_fields() {
		// These eight fields are parsed for fidelity but have no Quadlet mapping;
		// they must each warn so nothing is silently dropped from the export.
		let yaml = r#"
services:
  s:
    image: x
    cpu_count: 2
    cpu_percent: 50
    attach: false
    develop:
      watch:
        - path: ./src
          action: sync
          target: /app
    credential_spec:
      file: cred.json
    isolation: process
    provider:
      type: terraform
    use_api_socket: true
"#;
		let file = parse_str(yaml).unwrap();
		let joined = generate_at(&file, "proj", std::path::Path::new("/srv/app"))
			.warnings
			.join("\n");
		for field in [
			"cpu_count",
			"cpu_percent",
			"attach",
			"develop",
			"credential_spec",
			"isolation",
			"provider",
			"use_api_socket",
		] {
			assert!(
				joined.contains(field),
				"missing warning for {field}; got:\n{joined}"
			);
		}
	}

	#[test]
	fn clean_service_warns_about_nothing() {
		let yaml = r#"
services:
  web:
    image: nginx:1.27
"#;
		let file = parse_str(yaml).unwrap();
		assert!(generate_at(&file, "proj", std::path::Path::new("/srv/app"))
			.warnings
			.is_empty());
	}
}
