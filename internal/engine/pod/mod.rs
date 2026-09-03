//! Engine glue for the `x-podman-pod` extension.
//!
//! Owns the pod's request/response wiring (create, inspect, remove), the
//! pre-flight validation the `up`/`create` paths run before any container
//! is created, and the config hash that decides whether an existing pod
//! must be recreated.

mod ensure;
mod hash;
mod validate;

#[cfg(test)]
#[path = "validate_tests.rs"]
mod validate_tests;

#[cfg(all(test, unix))]
#[path = "engine_tests.rs"]
mod engine_tests;

pub(crate) use hash::pod_config_hash;
pub(crate) use validate::validate_pod_or_refuse;

// `Engine::ensure_pod` lives on the impl in `ensure.rs`; the free
// `validate_pod_or_refuse` is the pre-flight entry the `up`/`create` paths
// call. Re-exported here so callers see one `pod` module, not three.

use crate::libpod::types::pod::PodSpecGenerator;
use crate::ports::ParsedPort;

/// The label the engine stamps on every pod it owns, used by `down` and
/// `down --remove-orphans` to find the pod.
pub(super) const POD_PROJECT_LABEL: &str = "podup.project";

/// The label that records the hash of the pod's port set, network set and
/// host entries. A change in any of these three means the pod has to be
/// recreated.
pub(super) const POD_HASH_LABEL: &str = "podup.pod-config-hash";

/// Build the `hostadd` list: one `<service>:127.0.0.1` entry per service, so
/// `db:5432` written in a compose file resolves to the shared namespace the
/// way it resolves on a compose project network.
pub(super) fn hostadd_for_services<I, S>(services: I) -> Vec<String>
where
	I: IntoIterator<Item = S>,
	S: AsRef<str>,
{
	let mut entries: Vec<String> = services
		.into_iter()
		.map(|name| format!("{}:127.0.0.1", name.as_ref()))
		.collect();
	entries.sort();
	entries
}

/// Build the `portmappings` list: every service's parsed ports, in sorted
/// order so the wire bytes are deterministic across runs. Sorted by
/// `(host_ip, host_port, container_port, protocol)`.
pub(super) fn portmappings_for_services<I>(
	services_ports: I,
) -> Vec<crate::libpod::types::container::PortMapping>
where
	I: IntoIterator<Item = Vec<ParsedPort>>,
{
	let mut ports: Vec<ParsedPort> = services_ports.into_iter().flatten().collect();
	ports.sort_by(|a, b| {
		(&a.host_ip, a.host_port, a.container_port, &a.protocol).cmp(&(
			&b.host_ip,
			b.host_port,
			b.container_port,
			&b.protocol,
		))
	});
	crate::ports::to_libpod(&ports)
}

/// Build the `networks` map for a pod: every non-external network the file
/// declares (by its project-prefixed name) plus the external ones by their
/// own name, all attached with the same empty `PerNetworkOptions` map. The
/// infra container carries the same attachment list as every joined
/// service would, so a service that pinned `aliases:` would still see its
/// name on the network.
/// The `userns_mode` every service agrees on, applied to the pod. Validation
/// refused the project before this runs when the services disagree, so the
/// first service's value is the project's.
pub(super) fn pod_userns(file: &crate::compose::types::ComposeFile) -> Option<&str> {
	file.services
		.values()
		.find_map(|s| s.userns_mode.as_deref())
}

pub(super) fn pod_networks(
	file: &crate::compose::types::ComposeFile,
	project: &str,
) -> std::collections::HashMap<String, crate::libpod::types::container::PerNetworkOptions> {
	let mut nets: std::collections::HashMap<
		String,
		crate::libpod::types::container::PerNetworkOptions,
	> = std::collections::HashMap::new();
	for (key, config) in &file.networks {
		let external = config.as_ref().and_then(|c| c.external).unwrap_or(false);
		let name = if external {
			config
				.as_ref()
				.and_then(|c| c.name.clone())
				.unwrap_or_else(|| key.clone())
		} else {
			crate::engine::network::resolve_network_name(key, file, project)
		};
		nets.entry(name).or_default();
	}
	nets
}

/// Builds the pod request with a pre-computed hash so the recreate
/// path can keep the hash it just decided to set.
pub(super) fn build_pod_spec_with_hash(
	project: &str,
	file: &crate::compose::types::ComposeFile,
	parsed_ports: &[Vec<ParsedPort>],
	hash: &str,
) -> PodSpecGenerator {
	let mut labels = std::collections::HashMap::new();
	labels.insert(POD_PROJECT_LABEL.to_string(), project.to_string());
	labels.insert(POD_HASH_LABEL.to_string(), hash.to_string());
	let networks = pod_networks(file, project);
	PodSpecGenerator {
		name: project.to_string(),
		labels,
		shared_namespaces: vec!["net".to_string()],
		portmappings: portmappings_for_services(parsed_ports.iter().cloned()),
		netns: if networks.is_empty() {
			None
		} else {
			Some(crate::libpod::types::container::Namespace::new("bridge"))
		},
		networks,
		hostadd: hostadd_for_services(file.services.keys()),
		userns: pod_userns(file).map(crate::libpod::types::container::Namespace::parse),
	}
}
