//! Lifecycle and dependency types: `depends_on:`, `healthcheck:`, `restart:`, and lifecycle hooks.
//!
//! [`DependsOn`] models the service dependency graph — either a simple name list
//! or a map with per-dependency [`ServiceCondition`] semantics. [`HealthCheck`]
//! covers the inline healthcheck definition. [`RestartPolicy`] parses the
//! `restart:` string field. [`LifecycleHook`] is used for `post_start:` and
//! `pre_stop:` hook entries.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::env::EnvVars;
use super::primitives::Command;

/// `depends_on:` value — absent, a bare list of service names, or a map of service name to condition.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(untagged)]
pub enum DependsOn {
	/// No dependencies declared.
	#[default]
	Empty,
	/// Short form: a bare list of service names.
	List(Vec<String>),
	/// Long form: per-dependency conditions keyed by service name.
	Map(IndexMap<String, DependsOnCondition>),
}

impl DependsOn {
	/// Returns the names of all dependency services.
	pub fn service_names(&self) -> Vec<String> {
		match self {
			DependsOn::Empty => vec![],
			DependsOn::List(v) => v.clone(),
			DependsOn::Map(m) => m.keys().cloned().collect(),
		}
	}

	/// Returns the start condition for a dependency, defaulting to `service_started`.
	pub fn condition_for(&self, service: &str) -> ServiceCondition {
		match self {
			DependsOn::Map(m) => m
				.get(service)
				.map(|c| c.condition.clone())
				.unwrap_or(ServiceCondition::ServiceStarted),
			_ => ServiceCondition::ServiceStarted,
		}
	}

	/// Returns whether the dependent should restart when this dependency restarts.
	pub fn restart_for(&self, service: &str) -> bool {
		match self {
			DependsOn::Map(m) => m.get(service).and_then(|c| c.restart).unwrap_or(false),
			_ => false,
		}
	}

	/// Returns whether the dependency is required, defaulting to `true`.
	pub fn required_for(&self, service: &str) -> bool {
		match self {
			DependsOn::Map(m) => m.get(service).and_then(|c| c.required).unwrap_or(true),
			_ => true,
		}
	}
}

/// Long-form per-dependency entry in `depends_on:` — holds the condition and restart flag.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DependsOnCondition {
	/// Condition the dependency must reach before the dependent starts.
	pub condition: ServiceCondition,
	/// Whether the dependent restarts when this dependency is restarted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub restart: Option<bool>,
	/// Whether the dependency must be present; `true` if absent.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub required: Option<bool>,
}

/// Condition a dependency must satisfy before the dependent service starts.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceCondition {
	/// The dependency container has started.
	#[default]
	ServiceStarted,
	/// The dependency container has passed its health check.
	ServiceHealthy,
	/// The dependency container has exited successfully.
	ServiceCompletedSuccessfully,
}

/// Inline `healthcheck:` block. `disable: true` overrides any inherited health check.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct HealthCheck {
	/// Command run to determine container health.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub test: Option<Command>,
	/// Time between health check runs (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub interval: Option<String>,
	/// Maximum time a single check may run before failing (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub timeout: Option<String>,
	/// Consecutive failures before the container is marked unhealthy.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub retries: Option<u32>,
	/// Grace period after start before failures count (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub start_period: Option<String>,
	/// Check interval used during the start period (duration string).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub start_interval: Option<String>,
	/// Whether to disable any inherited health check.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub disable: Option<bool>,
	/// Unrecognized keys preserved verbatim for round-tripping.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub unknown: IndexMap<String, serde_yaml::Value>,
}

/// The Compose Spec extension key carrying Podman's `--health-on-failure`.
///
/// `x-` is the spec's reserved prefix for extensions: docker compose ignores an
/// unknown `x-` key rather than erroring, so a file using this stays a valid
/// compose file and still runs under docker — it just does not act on a sick
/// container there. That is the whole reason for the prefix rather than a bare
/// `on_failure`, which would make the file podup-only.
pub const X_PODMAN_ON_FAILURE: &str = "x-podman-on-failure";

/// What Podman does when a container's healthcheck flips to unhealthy.
///
/// The Compose Spec has no equivalent: a compose healthcheck detects a sick
/// container and does nothing about it. A restart policy does not cover this —
/// it reacts to the process exiting, not to the container being unhealthy — so
/// an app that hangs without dying stays in rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthOnFailure {
	/// Mark unhealthy and take no further action. Podman's default.
	None,
	/// Kill the container.
	Kill,
	/// Restart the container.
	Restart,
	/// Stop the container.
	Stop,
}

impl std::str::FromStr for HealthOnFailure {
	type Err = String;

	fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
		match value {
			"none" => Ok(Self::None),
			"kill" => Ok(Self::Kill),
			"restart" => Ok(Self::Restart),
			"stop" => Ok(Self::Stop),
			other => Err(format!(
				"invalid {X_PODMAN_ON_FAILURE} value {other:?} (expected one of: none, kill, restart, stop)"
			)),
		}
	}
}

impl HealthOnFailure {
	/// The Quadlet `HealthOnFailure=` value.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::None => "none",
			Self::Kill => "kill",
			Self::Restart => "restart",
			Self::Stop => "stop",
		}
	}
}

impl HealthCheck {
	/// The `x-podman-on-failure` extension, parsed and validated.
	///
	/// Spec-defined keys are typed fields on this struct; a Podman extension
	/// lives in `unknown` (where `#[serde(flatten)]` already preserves it for
	/// round-tripping) and is read through here. That split is deliberate — it
	/// keeps the type honest about which keys are portable.
	///
	/// `Err` for a value that is not one of Podman's four actions, so a typo is
	/// rejected rather than silently doing nothing.
	pub fn podman_on_failure(&self) -> std::result::Result<Option<HealthOnFailure>, String> {
		let Some(raw) = self.unknown.get(X_PODMAN_ON_FAILURE) else {
			return Ok(None);
		};
		let Some(text) = raw.as_str() else {
			return Err(format!(
				"{X_PODMAN_ON_FAILURE} must be a string (expected one of: none, kill, restart, stop)"
			));
		};
		text.parse().map(Some)
	}

	/// Returns whether the health check is disabled, either via `disable: true` or a `NONE` test.
	pub fn is_disabled(&self) -> bool {
		if self.disable.unwrap_or(false) {
			return true;
		}
		matches!(&self.test, Some(Command::Exec(v)) if v.len() == 1 && v[0].eq_ignore_ascii_case("NONE"))
	}
}

/// A single `post_start` or `pre_stop` lifecycle hook entry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LifecycleHook {
	/// Command executed for the hook.
	pub command: Command,
	/// User the hook command runs as.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub user: Option<String>,
	/// Whether the hook runs with elevated privileges.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub privileged: Option<bool>,
	/// Working directory for the hook command.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub working_dir: Option<String>,
	/// Environment variables set for the hook command.
	#[serde(default)]
	pub environment: EnvVars,
}

/// Service-level `restart:` policy — `no`, `always`, `unless-stopped`, or `on-failure` (with optional max-retries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPolicy {
	/// Never restart the container.
	No,
	/// Always restart the container when it stops.
	Always,
	/// Restart on non-zero exit, up to `max_attempts` times if set.
	OnFailure {
		/// Maximum restart attempts; unlimited if absent.
		max_attempts: Option<u32>,
	},
	/// Restart unless the container was explicitly stopped.
	UnlessStopped,
}

impl Serialize for RestartPolicy {
	// Emit the compose-spec string form so `config` output round-trips back
	// through `Deserialize` (the derived form would emit `UnlessStopped`).
	fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		let s = match self {
			RestartPolicy::No => "no".to_string(),
			RestartPolicy::Always => "always".to_string(),
			RestartPolicy::UnlessStopped => "unless-stopped".to_string(),
			RestartPolicy::OnFailure {
				max_attempts: Some(n),
			} => format!("on-failure:{n}"),
			RestartPolicy::OnFailure { max_attempts: None } => "on-failure".to_string(),
		};
		serializer.serialize_str(&s)
	}
}

impl<'de> Deserialize<'de> for RestartPolicy {
	fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let s = String::deserialize(deserializer)?;
		match s.as_str() {
			"no" => Ok(RestartPolicy::No),
			"always" => Ok(RestartPolicy::Always),
			"unless-stopped" => Ok(RestartPolicy::UnlessStopped),
			"on-failure" => Ok(RestartPolicy::OnFailure { max_attempts: None }),
			s if s.starts_with("on-failure:") => {
				let n = s["on-failure:".len()..]
					.parse::<u32>()
					.map_err(serde::de::Error::custom)?;
				Ok(RestartPolicy::OnFailure {
					max_attempts: Some(n),
				})
			}
			other => Err(serde::de::Error::custom(format!(
				"invalid restart policy: {other}"
			))),
		}
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
