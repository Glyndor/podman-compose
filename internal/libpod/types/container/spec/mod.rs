//! SpecGenerator and all nested request types for container creation.

use std::collections::HashMap;

use serde::Serialize;

mod parts;
pub use parts::*;

// ---------------------------------------------------------------------------
// SpecGenerator: container create request
// ---------------------------------------------------------------------------

/// Full container specification sent to `POST /libpod/containers/create`.
#[derive(Serialize, Default)]
pub struct SpecGenerator {
	/// Container name.
	pub name: String,
	/// Image reference to run (name:tag or digest).
	pub image: String,

	/// Name of an existing pod to attach this container to. When set, the
	/// container joins the pod's network namespace (the pod publishes any host
	/// ports itself, so the container's own `portmappings`/`networks`/`netns`
	/// stay empty in pod mode). Wire name is `pod`; without the rename the
	/// field would be silently dropped.
	#[serde(rename = "pod", skip_serializing_if = "Option::is_none")]
	pub pod: Option<String>,

	/// Command (`CMD`) override; replaces the image's default command.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub command: Option<Vec<String>>,

	/// Entrypoint override; replaces the image's `ENTRYPOINT`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub entrypoint: Option<Vec<String>>,

	/// Environment variables to set in the container.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub env: HashMap<String, String>,

	/// Allocate a pseudo-TTY for the container.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub terminal: Option<bool>,

	/// Keep stdin open for the container.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub stdin: Option<bool>,

	/// User to run as (`name`, `uid`, `uid:gid`, or `name:group`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub user: Option<String>,

	/// Working directory inside the container.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub work_dir: Option<String>,

	/// Stop signal as a numeric `syscall.Signal`. libpod rejects a string here
	/// with HTTP 500, so the compose `stop_signal:` name is resolved to its
	/// integer number before being sent.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub stop_signal: Option<i64>,

	/// Graceful stop timeout, in **seconds**, before SIGKILL.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub stop_timeout: Option<u64>,

	/// Container hostname.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub hostname: Option<String>,

	/// Container NIS domain name.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub domainname: Option<String>,

	// Labels and annotations
	/// Container labels.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub labels: HashMap<String, String>,

	/// OCI annotations attached to the container.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub annotations: HashMap<String, String>,

	// Capabilities and security
	/// Linux capabilities to add (without the `CAP_` prefix, e.g. `"NET_ADMIN"`).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub cap_add: Vec<String>,

	/// Linux capabilities to drop (without the `CAP_` prefix).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub cap_drop: Vec<String>,

	/// Run the container privileged (disables most isolation).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub privileged: Option<bool>,

	/// Mount the container's root filesystem read-only.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub read_only_filesystem: Option<bool>,

	/// SELinux options (compose `security_opt` `label=...`); Podman's
	/// SpecGenerator has no `security_opt` field, so the compose list is
	/// decomposed into these dedicated fields. A plain `security_opt` array is
	/// silently ignored, so every option (incl. no-new-privileges/seccomp/apparmor)
	/// would otherwise be dropped.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub selinux_opts: Vec<String>,

	/// AppArmor profile name to apply.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub apparmor_profile: Option<String>,

	/// Path to a seccomp profile JSON file (or `"unconfined"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub seccomp_profile_path: Option<String>,

	/// Set the `no_new_privs` flag, preventing privilege escalation.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub no_new_privileges: Option<bool>,

	/// Additional `/proc` and `/sys` paths to mask (hide).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub mask: Vec<String>,

	/// Paths to unmask (un-hide), reversing default masks; `"ALL"` unmasks all.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub unmask: Vec<String>,

	/// Kernel `sysctl` parameters to set for the container.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub sysctl: HashMap<String, String>,

	// Networking
	/// Ports to expose without publishing; key = port number, value = protocol
	/// (`"tcp"`/`"udp"`).
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub expose: HashMap<u16, String>,

	/// Published port mappings.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub portmappings: Vec<PortMapping>,

	/// Networks to connect to; key = network name, value = per-connection options.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub networks: HashMap<String, PerNetworkOptions>,

	/// Network namespace mode.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub netns: Option<Namespace>,

	/// Extra `/etc/hosts` entries (`"host:ip"`). Podman's SpecGenerator names this
	/// field `hostadd` (there is no `extra_hosts` key); without the rename every
	/// extra_hosts entry is silently dropped.
	#[serde(rename = "hostadd", skip_serializing_if = "Vec::is_empty", default)]
	pub extra_hosts: Vec<String>,

	/// Custom DNS server addresses.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub dns_server: Vec<String>,

	/// DNS search domains.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub dns_search: Vec<String>,

	/// DNS resolver options (`/etc/resolv.conf` `options` lines).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub dns_option: Vec<String>,

	// Volumes and mounts
	/// OCI mounts (bind/tmpfs/...) attached to the container.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub mounts: Vec<Mount>,

	/// Named-volume attachments.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub volumes: Vec<NamedVolume>,

	/// Container names/IDs to inherit volume mounts from.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub volumes_from: Vec<String>,

	/// Podman-native secrets (also used for external configs): each references an
	/// existing `podman secret` by name and is mounted into the container.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub secrets: Vec<Secret>,

	// Namespace modes
	/// User namespace mode.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub userns: Option<Namespace>,

	/// PID namespace mode.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pidns: Option<Namespace>,

	/// IPC namespace mode.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ipcns: Option<Namespace>,

	/// UTS namespace mode.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub utsns: Option<Namespace>,

	/// Cgroup namespace mode.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cgroupns: Option<Namespace>,

	/// Parent cgroup path under which the container's cgroup is created.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cgroup_parent: Option<String>,

	// Resource limits
	/// CPU/memory/pids/block-I/O resource limits.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub resource_limits: Option<LinuxResources>,

	/// POSIX rlimits. Podman's SpecGenerator names this field `r_limits`; without
	/// the rename every ulimits entry is silently dropped. The per-element shape
	/// (`{type, soft, hard}`) already matches Podman's POSIXRlimit.
	#[serde(rename = "r_limits", skip_serializing_if = "Vec::is_empty", default)]
	pub ulimits: Vec<Ulimit>,

	/// Size of `/dev/shm`, in **bytes**.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub shm_size: Option<i64>,

	// Healthcheck
	/// Main healthcheck configuration.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub healthconfig: Option<HealthConfig>,

	/// What Podman does when the healthcheck flips to unhealthy (Podman 5's
	/// `--health-on-failure`). Podman's key is `health_check_on_failure_action`.
	#[serde(
		rename = "health_check_on_failure_action",
		skip_serializing_if = "Option::is_none"
	)]
	pub health_check_on_failure_action: Option<HealthCheckOnFailureAction>,

	/// Separate startup-phase healthcheck (Podman 5's `--health-startup-*`).
	/// Podman's key is `startupHealthConfig`.
	#[serde(
		rename = "startupHealthConfig",
		skip_serializing_if = "Option::is_none"
	)]
	pub startup_health_config: Option<StartupHealthCheck>,

	// Logging
	/// Log driver configuration.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub log_configuration: Option<LogConfig>,

	// Init process
	/// Run an init process as PID 1 to reap zombies and forward signals.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub init: Option<bool>,

	// Restart policy
	/// Restart policy name (`"no"`, `"on-failure"`, `"always"`, `"unless-stopped"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub restart_policy: Option<String>,

	/// Maximum restart attempts (only meaningful for `on-failure`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub restart_tries: Option<u64>,

	/// Device nodes to expose. Podman 5.x has no SpecGenerator CDI field; CDI
	/// device names (e.g. `nvidia.com/gpu=all`) are recognized by ExtractCDIDevices
	/// from this array by their qualified path, so they are appended here as
	/// `LinuxDevice` entries.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub devices: Vec<LinuxDevice>,

	/// Cgroup device access rules. Podman expects structured rules
	/// ([`LinuxDeviceCgroup`]), not strings; a string array would fail to
	/// deserialize.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub device_cgroup_rule: Vec<LinuxDeviceCgroup>,

	// Groups
	/// Additional supplementary groups (names or GIDs) for the container process.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub groups: Vec<String>,

	// OOM
	/// OOM-killer score adjustment, `-1000`..=`1000`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub oom_score_adj: Option<i64>,

	// Runtime
	/// OCI runtime to use (e.g. `"crun"`, `"runc"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub runtime: Option<String>,

	/// Legacy container links (deprecated in rootless, accepted but may be ignored).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub links: Vec<String>,

	// Platform selection
	/// Target image CPU architecture (e.g. `"amd64"`, `"arm64"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub image_arch: Option<String>,

	/// Target image OS (e.g. `"linux"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub image_os: Option<String>,

	// Volume image handling
	/// How image-defined `VOLUME`s are handled (e.g. `"anonymous"`, `"tmpfs"`,
	/// `"ignore"`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub image_volume_mode: Option<String>,

	// Storage options
	/// Storage-driver options for the container's root filesystem.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub storage_opts: HashMap<String, String>,
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
