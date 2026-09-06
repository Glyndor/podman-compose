//! Nested request types referenced by [`SpecGenerator`](super::SpecGenerator).

use std::collections::HashMap;

use serde::Serialize;

// ---------------------------------------------------------------------------
// Nested types
// ---------------------------------------------------------------------------

/// Port mapping for SpecGenerator.
#[derive(Serialize, Default)]
pub struct PortMapping {
	/// Container-side port to publish.
	pub container_port: u16,

	/// Host-side port to bind; when absent Podman auto-assigns one.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub host_port: Option<u16>,

	/// Host interface address to bind on; empty means all interfaces.
	#[serde(skip_serializing_if = "String::is_empty", default)]
	pub host_ip: String,

	/// Transport protocol (`"tcp"` or `"udp"`).
	pub protocol: String,

	/// Number of ports to map starting from `container_port` (range mapping).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub range: Option<u16>,
}

/// A Podman-native secret attached to a container create spec, equivalent to
/// `podman run --secret`. Mirrors the libpod `Secret` type, which carries no
/// JSON tags upstream, so the wire keys are PascalCase (`Source`, `Target`, …).
/// `Source` names an existing Podman secret; `Target` is the mount destination
/// (a bare name lands under `/run/secrets/`, an absolute path is used as-is).
#[derive(Serialize, Default)]
pub struct Secret {
	/// Name of an existing Podman secret to attach.
	#[serde(rename = "Source")]
	pub source: String,

	/// Mount destination: a bare name lands under `/run/secrets/`, an absolute
	/// path is used as-is.
	#[serde(rename = "Target", skip_serializing_if = "Option::is_none")]
	pub target: Option<String>,

	/// Owner UID of the mounted secret file.
	#[serde(rename = "UID", skip_serializing_if = "Option::is_none")]
	pub uid: Option<u32>,

	/// Owner GID of the mounted secret file.
	#[serde(rename = "GID", skip_serializing_if = "Option::is_none")]
	pub gid: Option<u32>,

	/// File mode (permission bits) of the mounted secret, e.g. `0o400`.
	#[serde(rename = "Mode", skip_serializing_if = "Option::is_none")]
	pub mode: Option<u32>,
}

/// Per-network connection options (for SpecGenerator `networks` map).
#[derive(Serialize, Default)]
pub struct PerNetworkOptions {
	/// Additional DNS names the container is reachable by on this network.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub aliases: Vec<String>,

	/// Fixed IP addresses to assign on this network.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub static_ips: Vec<String>,

	/// Fixed MAC address for the container's interface on this network.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub static_mac: Option<String>,

	/// Name to give the container's network interface on this network.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub interface_name: Option<String>,

	/// Driver-specific per-connection options.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver_opts: Option<HashMap<String, String>>,
}

/// Linux network/pid/ipc/uts/cgroup namespace specification.
#[derive(Serialize, Clone)]
pub struct Namespace {
	/// Namespace mode (e.g. `"host"`, `"private"`, `"container"`, `"none"`).
	pub nsmode: String,

	/// Mode-dependent target, e.g. the container ID for `nsmode == "container"`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub value: Option<String>,
}

impl Namespace {
	/// Build a namespace with the given mode and no associated value.
	pub fn new(mode: impl Into<String>) -> Self {
		Self {
			nsmode: mode.into(),
			value: None,
		}
	}

	/// Build a `container:<id>` namespace sharing another container's namespace.
	pub fn container(id: impl Into<String>) -> Self {
		Self {
			nsmode: "container".into(),
			value: Some(id.into()),
		}
	}

	/// Parse a compose-style namespace string.
	///
	/// `"container:name"` → `{ nsmode: "container", value: "name" }`.
	/// Anything else → `{ nsmode: mode, value: None }`.
	pub fn parse(mode: impl Into<String>) -> Self {
		let mode = mode.into();
		if let Some(id) = mode.strip_prefix("container:") {
			Self::container(id)
		} else {
			Self::new(mode)
		}
	}
}

/// OCI mount specification for SpecGenerator.
#[derive(Serialize, Default)]
pub struct Mount {
	/// OCI mount type (e.g. `"bind"`, `"tmpfs"`, `"volume"`).
	#[serde(rename = "type")]
	pub mount_type: String,

	/// Host source path or tmpfs source; absent for anonymous tmpfs mounts.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,

	/// Absolute mount path inside the container.
	pub destination: String,

	/// OCI mount options (e.g. `"ro"`, `"rbind"`, `"nosuid"`).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub options: Vec<String>,
}

/// Named volume attachment for SpecGenerator (goes in `volumes`, not `mounts`).
#[derive(Serialize, Default)]
pub struct NamedVolume {
	/// Name of the Podman volume to attach.
	#[serde(rename = "Name")]
	pub name: String,

	/// Absolute mount path inside the container.
	#[serde(rename = "Dest")]
	pub dest: String,

	/// Mount options applied to the volume (e.g. `"ro"`, `"z"`).
	#[serde(rename = "Options", skip_serializing_if = "Vec::is_empty", default)]
	pub options: Vec<String>,

	/// Mount only this sub-directory of the volume (compose `volume.subpath`).
	#[serde(rename = "SubPath", skip_serializing_if = "Option::is_none", default)]
	pub sub_path: Option<String>,
}

/// Linux OCI resource limits for SpecGenerator.
#[derive(Serialize, Default)]
pub struct LinuxResources {
	/// Memory limits sub-block.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub memory: Option<LinuxMemory>,

	/// CPU limits sub-block.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu: Option<LinuxCPU>,

	/// Process-count (pids) limit sub-block.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pids: Option<LinuxPids>,

	/// Block I/O limits sub-block.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub block_io: Option<LinuxBlockIO>,

	/// GPU device access rules (maps `deploy.resources.reservations.devices`).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub devices: Vec<LinuxDeviceCgroup>,
}

/// Linux memory resource limits.
#[derive(Serialize, Default)]
pub struct LinuxMemory {
	/// Hard memory limit in **bytes** (`-1` disables the limit).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub limit: Option<i64>,

	/// Soft memory reservation (low-water mark) in **bytes**.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub reservation: Option<i64>,

	/// Total memory+swap limit in **bytes** (`-1` disables the limit).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub swap: Option<i64>,

	/// Swap tendency, `0`–`100` (kernel `memory.swappiness`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub swappiness: Option<u64>,

	/// When true, disables the OOM killer for the cgroup.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub disable_oom_killer: Option<bool>,
}

/// Linux CPU resource limits.
#[derive(Serialize, Default)]
pub struct LinuxCPU {
	/// Relative CPU weight (cgroup `cpu.shares`); proportional, not a hard cap.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub shares: Option<u64>,

	/// CPU time the cgroup may use per `period`, in **microseconds**
	/// (`-1` disables the quota).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub quota: Option<i64>,

	/// CFS scheduling period in **microseconds** that `quota` applies to.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub period: Option<u64>,

	/// Realtime scheduling period in **microseconds**.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub realtime_period: Option<u64>,

	/// Realtime runtime allowed per `realtime_period`, in **microseconds**.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub realtime_runtime: Option<i64>,

	/// CPU affinity as a cpuset string (e.g. `"0-3,5"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpus: Option<String>,
}

/// Linux pids (process count) limit.
#[derive(Serialize)]
pub struct LinuxPids {
	/// Maximum number of processes the cgroup may spawn (`-1` for unlimited).
	pub limit: i64,
}

/// Linux block I/O resource limits.
#[derive(Serialize, Default)]
pub struct LinuxBlockIO {
	/// Default block I/O weight, `10`–`1000` (cgroup `blkio.weight`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub weight: Option<u16>,

	/// Per-device block I/O weight overrides.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub weight_device: Vec<LinuxWeightDevice>,

	/// Per-device read-rate caps; each entry's `rate` is in **bytes per second**.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub throttle_read_bps_device: Vec<LinuxThrottleDevice>,

	/// Per-device write-rate caps; each entry's `rate` is in **bytes per second**.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub throttle_write_bps_device: Vec<LinuxThrottleDevice>,

	/// Per-device read-rate caps; each entry's `rate` is in **IO ops per second**.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub throttle_read_iops_device: Vec<LinuxThrottleDevice>,

	/// Per-device write-rate caps; each entry's `rate` is in **IO ops per second**.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub throttle_write_iops_device: Vec<LinuxThrottleDevice>,
}

/// Block device weight entry.
#[derive(Serialize)]
pub struct LinuxWeightDevice {
	/// Device major number.
	pub major: i64,
	/// Device minor number.
	pub minor: i64,

	/// Block I/O weight for this device, `10`–`1000`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub weight: Option<u16>,
}

/// Block device I/O throttle entry.
#[derive(Serialize)]
pub struct LinuxThrottleDevice {
	/// Device major number.
	pub major: i64,
	/// Device minor number.
	pub minor: i64,
	/// Throttle rate for this device. Unit depends on the containing list:
	/// **bytes per second** in `throttle_*_bps_device`, **IO ops per second**
	/// in `throttle_*_iops_device`.
	pub rate: u64,
}

/// cgroup device access rule (for GPU access via `deploy.resources`).
#[derive(Serialize)]
pub struct LinuxDeviceCgroup {
	/// Whether the rule allows (`true`) or denies (`false`) access.
	pub allow: bool,

	/// Device type: `"a"` (all), `"c"` (char), or `"b"` (block).
	#[serde(rename = "type", skip_serializing_if = "Option::is_none")]
	pub device_type: Option<String>,

	/// Device major number; absent means "all majors".
	#[serde(skip_serializing_if = "Option::is_none")]
	pub major: Option<i64>,

	/// Device minor number; absent means "all minors".
	#[serde(skip_serializing_if = "Option::is_none")]
	pub minor: Option<i64>,

	/// Access bits as any combination of `r` (read), `w` (write), `m` (mknod).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub access: Option<String>,
}

/// Process rlimit entry.
#[derive(Serialize)]
pub struct Ulimit {
	/// Resource name without the `RLIMIT_` prefix (e.g. `"nofile"`, `"nproc"`).
	#[serde(rename = "type")]
	pub ulimit_type: String,
	/// Soft limit (the value enforced until raised toward `hard`).
	pub soft: u64,
	/// Hard limit (the ceiling the soft limit may be raised to).
	pub hard: u64,
}

/// Container healthcheck configuration (same structure as Docker).
#[derive(Serialize, Default)]
pub struct HealthConfig {
	/// Probe command; `["CMD", ...]`, `["CMD-SHELL", "<cmd>"]`, or `["NONE"]`.
	#[serde(rename = "Test", skip_serializing_if = "Option::is_none")]
	pub test: Option<Vec<String>>,

	/// Time between probes, in **nanoseconds** (libpod expects ns).
	#[serde(rename = "Interval", skip_serializing_if = "Option::is_none")]
	pub interval: Option<i64>,

	/// Per-probe timeout, in **nanoseconds** (libpod expects ns).
	#[serde(rename = "Timeout", skip_serializing_if = "Option::is_none")]
	pub timeout: Option<i64>,

	/// Consecutive failures before the container is marked unhealthy.
	#[serde(rename = "Retries", skip_serializing_if = "Option::is_none")]
	pub retries: Option<i64>,

	/// Grace period before failures count, in **nanoseconds** (libpod expects ns).
	#[serde(rename = "StartPeriod", skip_serializing_if = "Option::is_none")]
	pub start_period: Option<i64>,

	/// Probe interval during the start period, in **nanoseconds** (libpod expects ns).
	#[serde(rename = "StartInterval", skip_serializing_if = "Option::is_none")]
	pub start_interval: Option<i64>,
}

/// Action Podman takes when a container's healthcheck transitions to unhealthy
/// (Podman 5's `--health-on-failure`).
///
/// Podman's `define.HealthCheckOnFailureAction` is an untagged `int` with no
/// custom JSON marshaller, so it travels the wire as a bare number. The explicit
/// discriminants below pin each variant to Podman's constant value; `Invalid`
/// (1) is Podman's error sentinel and is never emitted by a valid spec.
///
/// The variants are the complete Podman action set so the API surface is correct;
/// no compose key maps to them yet, hence `allow(dead_code)` until the
/// `--health-on-failure` plumbing lands.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum HealthCheckOnFailureAction {
	/// Take no action; only mark the container unhealthy (Podman's `none`, `0`).
	None = 0,
	/// Kill the container (Podman's `kill`, `2`).
	Kill = 2,
	/// Restart the container (Podman's `restart`, `3`).
	Restart = 3,
	/// Stop the container (Podman's `stop`, `4`).
	Stop = 4,
}

impl Serialize for HealthCheckOnFailureAction {
	fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
	where
		S: serde::Serializer,
	{
		// Wire form is the bare integer Podman assigns each action.
		serializer.serialize_u8(*self as u8)
	}
}

/// Startup-healthcheck configuration (Podman 5's `--health-startup-*`).
///
/// Podman's `define.StartupHealthCheck` embeds `Schema2HealthConfig` and adds a
/// `Successes` count; the embedded probe fields are flattened to the top level of
/// the `startupHealthConfig` object with their PascalCase keys (`Test`,
/// `Interval`, …), which is exactly [`HealthConfig`]'s wire shape, so it is
/// reused here via `#[serde(flatten)]`.
#[derive(Serialize, Default)]
pub struct StartupHealthCheck {
	/// The probe definition (test command, interval, timeout, retries, …),
	/// flattened so its fields sit alongside `Successes`.
	#[serde(flatten)]
	pub health_config: HealthConfig,

	/// Number of consecutive successes required before the container is considered
	/// started and the regular healthcheck takes over (`--health-startup-success`).
	#[serde(rename = "Successes", skip_serializing_if = "Option::is_none")]
	pub successes: Option<i64>,
}

/// Container log driver configuration.
#[derive(Serialize, Debug)]
pub struct LogConfig {
	/// Log driver name (e.g. `"json-file"`, `"journald"`, `"k8s-file"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,

	/// Maximum log file size in **bytes**. libpod reads rotation from this
	/// sibling field and ignores `max-size` keys inside [`options`](Self::options)
	/// (#1417). `-1` disables rotation, matching the Docker `--log-opt max-size=-1`
	/// convention.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub size: Option<i64>,

	/// Driver-specific options. The only keys libpod honours for the built-in
	/// log drivers are `path` and `tag` (see `man podman-run`); `max-file` is
	/// silently dropped by libpod regardless of where it appears, and `max-size`
	/// must travel as the typed [`size`](Self::size) field above.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub options: HashMap<String, String>,
}

/// Linux OCI device specification.
#[derive(Serialize)]
pub struct LinuxDevice {
	/// Device node path inside the container (or a CDI device name).
	pub path: String,

	/// Device type: `"c"` (char), `"b"` (block), `"p"` (FIFO), or `"u"`
	/// (unbuffered char).
	#[serde(rename = "type")]
	pub device_type: String,

	/// Device major number.
	pub major: i64,
	/// Device minor number.
	pub minor: i64,

	/// File mode (permission bits) of the created device node.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub file_mode: Option<u32>,

	/// Owner UID of the device node.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub uid: Option<u32>,

	/// Owner GID of the device node.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub gid: Option<u32>,
}
