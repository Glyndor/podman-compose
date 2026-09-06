//! Network configuration types for both top-level networks and per-service attachments.
//!
//! [`NetworkConfig`] describes a named network in the `networks:` top-level block.
//! [`ServiceNetworks`] is the per-service attachment: either a bare list of names
//! or a map with [`ServiceNetworkConfig`] options (aliases, IP, priority, etc.).

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::Labels;

/// `networks:` value at service level: absent, a bare list of network names, or a detailed map.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum ServiceNetworks {
	/// No networks declared.
	#[default]
	Empty,
	/// Short form: a bare list of network names.
	List(Vec<String>),
	/// Long form: per-network attachment options keyed by network name.
	Map(IndexMap<String, Option<ServiceNetworkConfig>>),
}

impl ServiceNetworks {
	/// Returns the names of all attached networks.
	pub fn names(&self) -> Vec<String> {
		match self {
			ServiceNetworks::Empty => vec![],
			ServiceNetworks::List(v) => v.clone(),
			ServiceNetworks::Map(m) => m.keys().cloned().collect(),
		}
	}

	/// Returns the attachment options for a network, if any are set.
	pub fn config_for(&self, name: &str) -> Option<&ServiceNetworkConfig> {
		match self {
			ServiceNetworks::Map(m) => m.get(name).and_then(|c| c.as_ref()),
			_ => None,
		}
	}
}

/// Per-network attachment options: aliases, static IPv4/IPv6, link-local addresses, and priority.
///
/// `#[non_exhaustive]` (3.0.0), like the volume option blocks: this grows a
/// field whenever the compose surface does, and each addition used to be a
/// breaking change for anyone building the literal. Construct via
/// `Default::default()` and assign the fields you need.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ServiceNetworkConfig {
	/// Additional network aliases the container is reachable by.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub aliases: Option<Vec<String>>,
	/// Static IPv4 address on this network.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ipv4_address: Option<String>,
	/// Static IPv6 address on this network.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ipv6_address: Option<String>,
	/// Link-local IP addresses for this attachment.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub link_local_ips: Vec<String>,
	/// Attachment priority used to order multiple networks.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub priority: Option<u32>,
	/// Static MAC address for this attachment.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mac_address: Option<String>,
	/// Driver-specific options for this attachment.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub driver_opts: HashMap<String, String>,
	/// Priority used to select the default gateway among networks.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub gw_priority: Option<u32>,
	/// Name of the interface created inside the container.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub interface_name: Option<String>,
}

/// Named network definition in the top-level `networks:` block.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct NetworkConfig {
	/// Network driver name; the runtime default is used if absent.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,
	/// Driver-specific options.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub driver_opts: HashMap<String, String>,
	/// Whether the network is externally managed and not created by podup.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub external: Option<bool>,
	/// Custom network name overriding the project-prefixed default.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Whether the network is isolated from external connectivity.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub internal: Option<bool>,
	/// Whether IPv6 is enabled on the network.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub enable_ipv6: Option<bool>,
	/// Whether IPv4 is enabled on the network.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub enable_ipv4: Option<bool>,
	/// Whether standalone containers may attach to the network.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub attachable: Option<bool>,
	/// IP address management configuration.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ipam: Option<IpamConfig>,
	/// Labels applied to the network.
	#[serde(default)]
	pub labels: Labels,
	/// Unrecognized keys preserved verbatim for round-tripping.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub unknown: IndexMap<String, serde_yaml::Value>,
}

/// `ipam:` block inside a top-level network definition.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct IpamConfig {
	/// IPAM driver name.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,
	/// Subnet/range pool definitions.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub config: Vec<IpamPool>,
	/// Driver-specific IPAM options.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub options: HashMap<String, String>,
	/// Unrecognized keys preserved verbatim for round-tripping.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub unknown: IndexMap<String, serde_yaml::Value>,
}

/// A single subnet/range entry within `ipam.config`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct IpamPool {
	/// Subnet in CIDR notation.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub subnet: Option<String>,
	/// Gateway address for the subnet.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub gateway: Option<String>,
	/// Range within the subnet to allocate addresses from.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ip_range: Option<String>,
	/// Reserved auxiliary addresses (`name -> address`).
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub aux_addresses: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "network_tests.rs"]
mod tests;
