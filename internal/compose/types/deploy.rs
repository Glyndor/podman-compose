//! Deployment configuration types for the `deploy:` service key.
//!
//! These types map to the Docker Swarm / Compose deploy spec and are used by
//! the engine to set resource limits, replica counts, restart policies, and
//! placement constraints. Most fields are optional; absent fields inherit the
//! container runtime defaults.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::Labels;

/// `deploy:` service key — resource limits, replica count, restart policies, and labels.
///
/// The engine uses `replicas`, `resources`, `restart_policy`, and `labels`.
/// Fields inherited from Docker Swarm (`mode`, `placement`, `update_config`,
/// `rollback_config`, `endpoint_mode`) are parsed so existing compose files
/// are accepted without error, but they have no effect on single-host Podman
/// and emit a warning at runtime.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DeployConfig {
	/// Desired number of container replicas.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub replicas: Option<u32>,
	/// Resource limits and reservations.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub resources: Option<ResourcesConfig>,
	/// Restart policy for the service's containers.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub restart_policy: Option<DeployRestartPolicy>,
	/// Rolling update configuration (Swarm-only; parsed but ignored).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub update_config: Option<DeployUpdateConfig>,
	/// Rollback configuration (Swarm-only; parsed but ignored).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub rollback_config: Option<DeployUpdateConfig>,
	/// Service endpoint mode (Swarm-only; parsed but ignored).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub endpoint_mode: Option<String>,
	/// Replication mode, `replicated` or `global` (Swarm-only; parsed but ignored).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mode: Option<String>,
	/// Labels applied to the service.
	#[serde(default)]
	pub labels: Labels,
	/// Placement constraints (Swarm-only; parsed but ignored).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub placement: Option<DeployPlacement>,
	/// Unrecognized keys preserved verbatim for round-tripping.
	#[serde(flatten, default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
	pub unknown: indexmap::IndexMap<String, serde_yaml::Value>,
}

/// Resource constraints under `deploy.resources:` — holds `limits` and `reservations`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ResourcesConfig {
	/// Hard upper bounds on resource usage.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub limits: Option<ResourceSpec>,
	/// Resources guaranteed to the service.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub reservations: Option<ResourceSpec>,
}

/// A single resource specification: CPU shares, memory limit, pids limit, and device access.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ResourceSpec {
	/// CPU limit expressed in cores (e.g. `0.5` or `"0.5"`); accepts both the
	/// unquoted-number and quoted-string spec forms.
	#[serde(
		default,
		deserialize_with = "super::primitives::deserialize_opt_string_or_number",
		skip_serializing_if = "Option::is_none"
	)]
	pub cpus: Option<String>,
	/// Memory limit as a size string (e.g. `"512M"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub memory: Option<String>,
	/// Maximum number of process IDs.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pids: Option<u64>,
	/// Device reservations such as GPUs.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub devices: Vec<DeviceReservation>,
}

// ---------------------------------------------------------------------------
// Device reservations (GPU / accelerators)
// ---------------------------------------------------------------------------

/// `deploy.resources.reservations.devices` — generic device reservation.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DeviceReservation {
	/// Required device capabilities (e.g. `gpu`, `compute`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub capabilities: Vec<String>,
	/// Number of matching devices to reserve, or `all`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub count: Option<CountOrAll>,
	/// Specific device IDs to reserve.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub device_ids: Vec<String>,
	/// Device driver name.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,
	/// Driver-specific options.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub options: HashMap<String, String>,
}

/// `count: all` or `count: N`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CountOrAll {
	/// String form, `"all"`.
	Named(String),
	/// Numeric device count.
	N(i64),
}

impl CountOrAll {
	/// Returns the count as an integer, with `-1` representing `all`.
	pub fn to_i64(&self) -> i64 {
		match self {
			CountOrAll::Named(_) => -1,
			CountOrAll::N(n) => *n,
		}
	}
}

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;

/// Restart policy under `deploy.restart_policy:` — distinct from the service-level `restart:` string.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DeployRestartPolicy {
	/// When to restart, e.g. `on-failure` or `any`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub condition: Option<String>,
	/// Delay between restart attempts (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub delay: Option<String>,
	/// Maximum restart attempts before giving up.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub max_attempts: Option<u32>,
	/// Window over which attempts are counted (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub window: Option<String>,
}

/// Update (and rollback) configuration — reused for both `deploy.update_config` and `deploy.rollback_config`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DeployUpdateConfig {
	/// Number of containers updated at once.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub parallelism: Option<u32>,
	/// Delay between updating batches (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub delay: Option<String>,
	/// Action taken if an update fails.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub failure_action: Option<String>,
	/// Time to monitor each task for failure (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub monitor: Option<String>,
	/// Failure ratio tolerated before the update is considered failed.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub max_failure_ratio: Option<f64>,
	/// Order of stopping old and starting new containers.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub order: Option<String>,
}

/// Placement constraints and preferences controlling which nodes a service's containers may run on.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DeployPlacement {
	/// Node constraints that must be satisfied.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub constraints: Vec<String>,
	/// Soft placement preferences.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub preferences: Vec<serde_yaml::Value>,
	/// Maximum replicas allowed on a single node.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub max_replicas_per_node: Option<u32>,
}
