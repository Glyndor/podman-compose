//! Hash of the pod's mutable surface: port set, network set, host entries.
//!
//! Only these three change the shape of a pod that cannot be edited in
//! place, so only these three contribute to the hash. Anything else (the
//! service command, the image, the labels) is a container-level concern
//! and is covered by the per-container `config_hash`.
//!
//! The three inputs are sorted before serialisation, so two projects with
//! the same networks/ports/hosts declared in different orders hash the
//! same. The serialisation is the same canonical form
//! [`crate::engine::container::config_hash`] uses (round-trip through
//! `serde_json::Value` so map key order is deterministic), so a project
//! whose network list is reordered by the parser does not flap the hash.

use sha2::{Digest, Sha256};

use crate::compose::types::ComposeFile;
use crate::ports::ParsedPort;

/// Hash the port set, network set and host entries into a stable 64-hex
/// string, the way the per-container `config_hash` hashes a service.
pub(crate) fn pod_config_hash(parsed_ports: &[Vec<ParsedPort>], file: &ComposeFile) -> String {
	let mut hasher = Sha256::new();

	// Sorted port set: every parsed port of every service, sorted by
	// (host_ip, host_port, container_port, protocol).
	let mut ports: Vec<ParsedPort> = parsed_ports.iter().flatten().cloned().collect();
	ports.sort_by(|a, b| {
		(&a.host_ip, a.host_port, a.container_port, &a.protocol).cmp(&(
			&b.host_ip,
			b.host_port,
			b.container_port,
			&b.protocol,
		))
	});
	let ports_value = serde_json::to_value(&ports).expect("ports serialise");
	hasher.update(b"ports");
	hash_canon(&mut hasher, &ports_value);

	// Sorted network set: the names podup will pass to the pod's `networks`
	// map (declared networks, by their resolved names; external networks by
	// their own name).
	let mut networks: Vec<String> = file
		.networks
		.iter()
		.map(|(key, config)| {
			let external = config.as_ref().and_then(|c| c.external).unwrap_or(false);
			if external {
				config
					.as_ref()
					.and_then(|c| c.name.clone())
					.unwrap_or_else(|| key.clone())
			} else {
				key.clone()
			}
		})
		.collect();
	networks.sort();
	let networks_value = serde_json::to_value(&networks).expect("networks serialise");
	hasher.update(b"networks");
	hash_canon(&mut hasher, &networks_value);

	// Sorted host entries: one `<service>:127.0.0.1` per service.
	let mut hosts: Vec<String> = file
		.services
		.keys()
		.map(|s| format!("{s}:127.0.0.1"))
		.collect();
	hosts.sort();
	let hosts_value = serde_json::to_value(&hosts).expect("hosts serialise");
	hasher.update(b"hosts");
	hash_canon(&mut hasher, &hosts_value);

	hasher
		.finalize()
		.iter()
		.map(|b| format!("{b:02x}"))
		.collect()
}

/// Canonicalise through `serde_json::Value` so map keys sort
/// lexicographically, then fold the bytes into `hasher`. Folding the
/// `Value` rather than the original struct is what `config_hash` does for
/// the same reason: a `HashMap`-backed field would otherwise emit bytes in
/// an iteration-dependent order and flap the hash on every parse.
fn hash_canon(hasher: &mut Sha256, value: &serde_json::Value) {
	let bytes = serde_json::to_vec(value).expect("Value serialises");
	hasher.update(&bytes);
}
