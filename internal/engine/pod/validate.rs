//! Pre-flight validation for the `x-podman-pod` extension.
//!
//! `up`/`create` call [`validate_pod_or_refuse`] before any container or
//! pod is created, so a configuration the pod cannot honour is reported
//! up front with a message naming the service and the offending key, not
//! mid-create as an opaque libpod 500.
//!
//! Refusals:
//!
//! - `network_mode` on any service: a pod already pins every container to
//!   one shared namespace, and `network_mode` would override that. Refuse
//!   rather than silently drop the field.
//! - Two services with divergent `networks:` sets: every service has to be
//!   on the same set (or declare none and get the project default). The
//!   pod's `networks` map is built from every declared network, and two
//!   services that disagree would either leave some unreachable or pull
//!   in extras. Per-service sets are checked against the first non-empty
//!   set seen.
//! - Two services publishing the same host port: pod mode hands the
//!   union of every service's `ports:` to the pod, and Podman rejects
//!   duplicate host ports with HTTP 500. Pre-empt it.
//! - Two services publishing the same port on different host IPs: the
//!   union collapses the two into the same Podman entry, and the per-IP
//!   binding is silently lost. Refuse so the user picks one or the other.

use indexmap::IndexSet;

use crate::compose::types::{ComposeFile, Service};

/// Pre-flight check: refuse any compose-file shape the pod cannot honour.
/// `Err` messages name the service and the offending key, so the user can
/// fix the source.
pub(crate) fn validate_pod_or_refuse(file: &ComposeFile) -> Result<(), String> {
	let services: Vec<(&str, &Service)> =
		file.services.iter().map(|(k, v)| (k.as_str(), v)).collect();

	// 1. `network_mode` on any service: rejected.
	for (name, service) in &services {
		if let Some(mode) = &service.network_mode {
			return Err(format!(
				"service \"{name}\": network_mode {mode:?} is incompatible with x-podman-pod; \
				 the pod pins every container to its shared namespace, so a per-service \
				 network_mode cannot be honoured"
			));
		}
	}

	// 2. Divergent networks: the first service that declares any network
	//    defines the canonical set, every other service with a non-empty
	//    `networks:` must equal it.
	let mut canonical: Option<IndexSet<String>> = None;
	for (name, service) in &services {
		let names: IndexSet<String> = service.networks.names().into_iter().collect();
		if names.is_empty() {
			continue;
		}
		match &canonical {
			None => canonical = Some(names),
			Some(c) if c != &names => {
				let c_list: Vec<&str> = c.iter().map(String::as_str).collect();
				let n_list: Vec<&str> = names.iter().map(String::as_str).collect();
				return Err(format!(
					"service \"{name}\": networks: [{n}] does not match the first service's \
					 networks: [{c}]; x-podman-pod requires every service to declare the same \
					 set of networks (or none, for the project default)",
					n = n_list.join(", "),
					c = c_list.join(", "),
				));
			}
			_ => {}
		}
	}

	// 3 & 4. Port collisions: same host_port on more than one service, and
	//    the same host_port bound to two different host IPs. Tracked by
	//    (host_port, protocol) so a TCP and UDP on the same host port do
	//    not falsely collide.
	// A pod has one user namespace and Podman refuses a member with its own,
	// so every service declares the same `userns_mode`, or none.
	let mut userns: Option<(&str, Option<&str>)> = None;
	for (name, service) in &services {
		let mode = service.userns_mode.as_deref();
		match userns {
			None => userns = Some((name, mode)),
			Some((first, first_mode)) if first_mode != mode => {
				return Err(format!(
					"service \"{name}\": userns_mode {} does not match service \"{first}\"'s {}; \
					 x-podman-pod gives the pod one user namespace, so every service declares \
					 the same userns_mode (or none)",
					mode.map_or("(unset)".to_string(), |m| format!("{m:?}")),
					first_mode.map_or("(unset)".to_string(), |m| format!("{m:?}")),
				));
			}
			_ => {}
		}
	}

	let mut host_port_owner: std::collections::HashMap<(u16, String), String> =
		std::collections::HashMap::new();
	for (name, service) in &services {
		let Ok(parsed) = crate::ports::parse_ports(&service.ports) else {
			// An invalid port is already reported by the per-service path; the
			// pod check is not the right place to re-report it.
			continue;
		};
		for p in &parsed {
			let host_port = match p.host_port {
				Some(n) if n != 0 => n,
				_ => continue,
			};
			let key = (host_port, p.protocol.clone());

			// 3. Duplicate host port.
			if let Some(prev) = host_port_owner.get(&key) {
				if prev != name {
					return Err(format!(
						"services \"{prev}\" and \"{name}\" both publish host port \
						 {host_port}/{}; x-podman-pod hands the union of every service's \
						 ports: to the pod, where duplicate host ports collide",
						p.protocol,
					));
				}
				continue;
			}
			host_port_owner.insert(key, name.to_string());
		}
	}

	Ok(())
}
