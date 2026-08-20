//! Warnings for service/network/secret fields that podup parses but cannot
//! translate. Split out of the diagnostics root so each collector stays small.

use crate::compose::types::{BuildConfig, ComposeFile, EnvFileEntry, PortMapping, VolumeMount};

/// Service fields that podup models but cannot honor on rootless Podman.
pub(super) fn ignored_service_fields(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		if def.cpu_count.is_some() {
			out.push(format!(
				"service '{service}': cpu_count is a Windows/Hyper-V control with no \
				 rootless Podman equivalent and is ignored"
			));
		}
		if def.cpu_percent.is_some() {
			out.push(format!(
				"service '{service}': cpu_percent is a Windows/Hyper-V control with no \
				 rootless Podman equivalent and is ignored"
			));
		}
		if def.credential_spec.is_some() {
			out.push(format!(
				"service '{service}': credential_spec is a Windows managed-service-account \
				 control with no rootless Podman equivalent and is not honored"
			));
		}
		if def.isolation.is_some() {
			out.push(format!(
				"service '{service}': isolation has no rootless Podman equivalent and is \
				 not honored"
			));
		}
		if def.provider.is_some() {
			out.push(format!(
				"service '{service}': provider delegates the service lifecycle to an \
				 external plugin that podup does not invoke; the service is not honored"
			));
		}
		if def.use_api_socket.is_some() {
			out.push(format!(
				"service '{service}': use_api_socket has no podup equivalent and is not \
				 honored"
			));
		}
		for entry in def.env_file.to_entries() {
			if let EnvFileEntry::Config {
				format: Some(fmt), ..
			} = entry
			{
				out.push(format!(
					"service '{service}': env_file format '{fmt}' is not honored; podup \
					 always parses env files as dotenv"
				));
			}
		}
	}
}

/// Top-level `models:` (Compose v2.38) — podup runs no model runner, so any
/// declared model is parsed for fidelity but not honored.
pub(super) fn ignored_models(file: &ComposeFile, out: &mut Vec<String>) {
	for name in file.models.keys() {
		out.push(format!(
			"model '{name}': podup runs no model runner, so the models element is not \
			 honored"
		));
	}
}

/// Long-form port fields podup parses but does not forward to Podman.
pub(super) fn ignored_port_fields(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		for port in &def.ports {
			if let PortMapping::Long { mode: Some(m), .. } = port {
				out.push(format!(
					"service '{service}': port mode '{m}' is a Swarm/ingress control \
					 with no single-host Podman equivalent and is ignored"
				));
			}
		}
	}
}

/// Warn when a service publishes a port on every host interface. The
/// compose-spec short form (`"5432:5432"`) and the long form with no
/// `host_ip` both bind on all interfaces, which exposes services the
/// operator thought were local-only (databases, admin UIs) to anything
/// reachable on the host's network. An explicit `host_ip` — including
/// `0.0.0.0` — is a decision taken and is not flagged, so flagging it
/// would only train the reader to ignore the warning.
pub(super) fn port_published_on_all_interfaces(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		for port in &def.ports {
			match port {
				PortMapping::Short(s) => {
					let no_proto = s.split('/').next().unwrap_or(s);
					// IPv6 form (`[::1]:host:container`) always carries an IP.
					if no_proto.starts_with('[') {
						continue;
					}
					let colon_count = no_proto.chars().filter(|&c| c == ':').count();
					// 0 colons = container-only (expose, not publish).
					// 2+ colons = ip:host:container (has IP).
					// 1 colon = host:container without IP.
					if colon_count == 1 {
						let mut parts = no_proto.split(':');
						let host = parts.next().unwrap_or("");
						let cont = parts.next().unwrap_or("");
						out.push(format!(
							"service '{service}': port {host} is published on every \
							 interface; use \"127.0.0.1:{host}:{cont}\" to keep it on \
							 the host"
						));
					}
				}
				PortMapping::Long {
					published: Some(p),
					host_ip,
					..
				} => {
					let explicit = host_ip.as_deref().is_some_and(|s| !s.trim().is_empty());
					if !explicit {
						let host = p.as_str_val();
						out.push(format!(
							"service '{service}': port {host} is published on every \
							 interface; use host_ip: \"127.0.0.1\" to keep it on the \
							 host"
						));
					}
				}
				PortMapping::Long {
					published: None, ..
				} => {
					// No `published` = port is exposed, not published on host.
				}
			}
		}
	}
}

/// Per-mount long-form volume options podup parses but does not forward.
pub(super) fn ignored_volume_mount_fields(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		for mount in &def.volumes {
			if let VolumeMount::Long {
				volume: Some(opts), ..
			} = mount
			{
				if opts.driver_config.is_some() {
					out.push(format!(
						"service '{service}' volume '{}': per-mount driver_config is not \
						 forwarded to Podman and is ignored",
						mount.target()
					));
				}
			}
		}
	}
}

/// Build options that exist only in BuildKit/buildx and have no libpod
/// build-API mapping. Honored fields are left untouched.
pub(super) fn ignored_build_fields(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		let Some(BuildConfig::Config {
			privileged,
			isolation,
			entitlements,
			provenance,
			sbom,
			ssh,
			..
		}) = &def.build
		else {
			continue;
		};
		let mut unmapped: Vec<&str> = Vec::new();
		if privileged.is_some() {
			unmapped.push("privileged");
		}
		if !ssh.is_empty() {
			unmapped.push("ssh");
		}
		if isolation.is_some() {
			unmapped.push("isolation");
		}
		if !entitlements.is_empty() {
			unmapped.push("entitlements");
		}
		if provenance.is_some() {
			unmapped.push("provenance");
		}
		if sbom.is_some() {
			unmapped.push("sbom");
		}
		for field in unmapped {
			out.push(format!(
				"service '{service}': build.{field} has no libpod build-API equivalent \
				 and is ignored"
			));
		}
	}
}

/// Network fields podup parses but does not forward to Podman.
pub(super) fn ignored_network_fields(file: &ComposeFile, out: &mut Vec<String>) {
	for (name, cfg) in &file.networks {
		if let Some(c) = cfg {
			if c.enable_ipv4.is_some() {
				out.push(format!(
					"network '{name}': enable_ipv4 is not forwarded; Podman networks \
					 enable IPv4 by default and expose no toggle"
				));
			}
			if let Some(ipam) = &c.ipam {
				if ipam
					.config
					.iter()
					.any(|pool| !pool.aux_addresses.is_empty())
				{
					out.push(format!(
						"network '{name}': ipam aux_addresses are not supported by Podman \
						 and are ignored"
					));
				}
			}
		}
	}
}

/// `deploy.restart_policy` sub-fields with no Podman restart-policy equivalent.
/// Only `condition` and `max_attempts` are honored (see container_config); Podman
/// has no first-class restart delay or attempt-counting window.
pub(super) fn ignored_restart_policy_fields(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		let Some(drp) = def.deploy.as_ref().and_then(|d| d.restart_policy.as_ref()) else {
			continue;
		};
		if drp.delay.is_some() {
			out.push(format!(
				"service '{service}': deploy.restart_policy.delay has no Podman equivalent \
				 and is ignored"
			));
		}
		if drp.window.is_some() {
			out.push(format!(
				"service '{service}': deploy.restart_policy.window has no Podman equivalent \
				 and is ignored"
			));
		}
	}
}

/// Per-service network attachment fields podup parses but cannot forward.
/// `gw_priority` has no Podman equivalent, so the engine drops it silently.
pub(super) fn ignored_service_network_fields(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		for name in def.networks.names() {
			if let Some(c) = def.networks.config_for(&name) {
				if c.gw_priority.is_some() {
					out.push(format!(
						"service '{service}' network '{name}': gw_priority is not supported \
						 by Podman and is ignored"
					));
				}
			}
		}
	}
}

/// Top-level secret/config driver fields. An external secret-store driver
/// (Vault, AWS SM, etc.) on a non-`external` definition is not honored — podup
/// only stages `file`/`content`/`environment` sources and routes `external:
/// true` to Podman-native secrets — so warn rather than mount nothing silently.
pub(super) fn ignored_secret_config_drivers(file: &ComposeFile, out: &mut Vec<String>) {
	for (name, cfg) in &file.secrets {
		if cfg.external != Some(true) {
			if cfg.driver.is_some() {
				out.push(format!(
					"secret '{name}': driver is an external secret-store plugin that podup \
					 does not invoke; the secret will not be staged"
				));
			}
			if cfg.template_driver.is_some() {
				out.push(format!(
					"secret '{name}': template_driver is not supported and is ignored"
				));
			}
		}
	}
	for (name, cfg) in &file.configs {
		if cfg.external != Some(true) {
			if cfg.driver.is_some() {
				out.push(format!(
					"config '{name}': driver is an external plugin that podup does not \
					 invoke; the config will not be staged"
				));
			}
			if cfg.template_driver.is_some() {
				out.push(format!(
					"config '{name}': template_driver is not supported and is ignored"
				));
			}
		}
	}
}
