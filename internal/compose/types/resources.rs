//! Resource limit and device types shared across service and deploy configuration.
//!
//! [`UlimitConfig`] maps to the `ulimits:` map — either a single value (soft==hard)
//! or an explicit soft/hard pair. [`BlkioConfig`] covers block I/O weight and rate
//! limits. [`GpuSpec`] handles the `gpus:` shorthand (`"all"` or a count).

use serde::{Deserialize, Serialize};

/// `ulimits:` entry — either a single integer (soft == hard) or an explicit `{soft, hard}` pair.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum UlimitConfig {
	/// Single value applied to both the soft and hard limit.
	Single(i64),
	/// Explicit soft and hard limit pair.
	Pair {
		/// Soft limit value.
		soft: i64,
		/// Hard limit value.
		hard: i64,
	},
}

impl UlimitConfig {
	/// Returns the soft limit.
	pub fn soft(&self) -> i64 {
		match self {
			UlimitConfig::Single(n) => *n,
			UlimitConfig::Pair { soft, .. } => *soft,
		}
	}

	/// Returns the hard limit.
	pub fn hard(&self) -> i64 {
		match self {
			UlimitConfig::Single(n) => *n,
			UlimitConfig::Pair { hard, .. } => *hard,
		}
	}
}

/// `blkio_config:` service field — controls I/O weight and per-device rate limits.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct BlkioConfig {
	/// Default block I/O weight for the service (10-1000).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub weight: Option<u16>,
	/// Per-device I/O weight overrides.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub weight_device: Vec<BlkioWeightDevice>,
	/// Per-device read rate limits in bytes/second.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub device_read_bps: Vec<BlkioRateDevice>,
	/// Per-device write rate limits in bytes/second.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub device_write_bps: Vec<BlkioRateDevice>,
	/// Per-device read rate limits in IOPS.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub device_read_iops: Vec<BlkioRateDevice>,
	/// Per-device write rate limits in IOPS.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub device_write_iops: Vec<BlkioRateDevice>,
}

/// Per-device I/O weight entry under `blkio_config.weight_device`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlkioWeightDevice {
	/// Host device path the weight applies to.
	pub path: String,
	/// I/O weight for the device (10-1000).
	pub weight: u16,
}

/// Per-device rate limit under `blkio_config` — used for both bps and iops limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlkioRateDevice {
	/// Host device path the limit applies to.
	pub path: String,
	/// Rate limit value, as a number or size string.
	pub rate: serde_yaml::Value,
}

impl BlkioRateDevice {
	/// Return rate as bytes/second (or IOPS as a plain integer).
	pub fn rate_value(&self) -> i64 {
		match &self.rate {
			serde_yaml::Value::Number(n) => n.as_i64().unwrap_or(0),
			serde_yaml::Value::String(s) => crate::size::parse_memory(s).unwrap_or(0),
			_ => 0,
		}
	}
}

/// Top-level service `gpus:` field. The spec allows the `all`/`N` shorthand and
/// a list of device-reservation objects (same shape as
/// `deploy.resources.reservations.devices`); both forms are accepted so a valid
/// compose file never fails to parse.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum GpuSpec {
	/// Shorthand string form, e.g. `"all"`.
	Named(String),
	/// Number of GPUs to expose.
	Count(u32),
	/// Explicit device reservation list.
	Devices(Vec<super::deploy::DeviceReservation>),
}

impl GpuSpec {
	/// -1 = all; positive = exact count. The device-list form is treated as
	/// "all" since it is not mapped to CDI from this field (use
	/// `deploy.resources.reservations.devices` for that).
	pub fn to_count(&self) -> i64 {
		match self {
			GpuSpec::Named(_) | GpuSpec::Devices(_) => -1,
			GpuSpec::Count(n) => *n as i64,
		}
	}
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
