//! [`Service`] struct — the central type representing a single compose service.
//!
//! Fields map 1-to-1 to the Docker Compose specification. Optional fields use
//! `Option<T>` so that absent keys are distinguishable from explicit nulls
//! during `extends:` merging.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::build::{BuildConfig, ExtendsConfig};
use super::deploy::DeployConfig;
use super::develop::DevelopConfig;
use super::network::ServiceNetworks;
use super::volume::{ServiceConfigRef, ServiceSecretRef, VolumeMount};
use super::{
	BlkioConfig, Command, DependsOn, EnvFile, EnvVars, GpuSpec, HealthCheck, Labels, LifecycleHook,
	LoggingConfig, PortMapping, RestartPolicy, StringOrList, Sysctls, UlimitConfig,
};

/// A single service definition.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct Service {
	/// `image:` — container image reference (`name[:tag|@digest]`) to run; if
	/// absent, `build:` must supply one.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub image: Option<String>,
	/// `build:` — build the image from source instead of (or in addition to)
	/// pulling `image:`; a string shorthand is the build context path.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub build: Option<BuildConfig>,
	/// `extends:` — inherit configuration from another service (optionally in
	/// another file); merged before this service's own keys are applied.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub extends: Option<ExtendsConfig>,
	/// `command:` — overrides the image `CMD`; string (shell-parsed) or list (exec
	/// form, no shell).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub command: Option<Command>,
	/// `entrypoint:` — overrides the image `ENTRYPOINT` and resets any image
	/// `CMD`; string (shell form) or list (exec form).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub entrypoint: Option<Command>,

	/// `ports:` — published host↔container port mappings (`[host:]container[/proto]`
	/// or long form); exposes the port outside the container network.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub ports: Vec<PortMapping>,
	/// `expose:` — ports made reachable to linked services only, without
	/// publishing them on the host.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub expose: Vec<String>,

	/// `environment:` — environment variables set in the container; map or
	/// `KEY=VALUE` list. Takes precedence over `env_file:`.
	#[serde(default)]
	pub environment: EnvVars,
	/// `env_file:` — file(s) of `KEY=VALUE` lines loaded into the environment;
	/// overridden by `environment:` on key collision.
	#[serde(default)]
	pub env_file: EnvFile,
	/// `volumes:` — bind mounts, named volumes, and anonymous volumes (short
	/// `src:dst[:opts]` or long form).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub volumes: Vec<VolumeMount>,
	/// `tmpfs:` — paths mounted as an in-memory tmpfs inside the container; single
	/// path or list.
	#[serde(default)]
	pub tmpfs: StringOrList,
	/// `volumes_from:` — mount all volumes from another service or container
	/// (`name[:ro|rw]`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub volumes_from: Vec<String>,
	/// `configs:` — top-level configs granted to this service, mounted as files
	/// (default `/<config-name>`) or exposed per the long form.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub configs: Vec<ServiceConfigRef>,
	/// `secrets:` — top-level secrets granted to this service, mounted under
	/// `/run/secrets/` (or the long-form target).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub secrets: Vec<ServiceSecretRef>,

	/// `networks:` — networks this service joins, with optional per-network
	/// aliases/IPs; absent means the project's default network.
	#[serde(default)]
	pub networks: ServiceNetworks,
	/// `hostname:` — the container's own hostname (its `/etc/hostname` and
	/// in-container `uname -n`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub hostname: Option<String>,
	/// `domainname:` — the container's NIS/DNS domain (fully-qualified
	/// hostname suffix).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub domainname: Option<String>,
	/// `mac_address:` — fixed MAC for the container's primary interface; a
	/// documented divergence under rootless Podman where it may be ignored.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mac_address: Option<String>,
	/// `links:` — legacy service links (`service[:alias]`) adding hostname
	/// entries; superseded by shared networks.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub links: Vec<String>,
	/// `external_links:` — link to containers outside this compose project
	/// (`container[:alias]`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub external_links: Vec<String>,
	/// `extra_hosts:` — extra `/etc/hosts` entries (`hostname:ip`); accepts the
	/// list or mapping YAML forms.
	#[serde(
		default,
		deserialize_with = "super::primitives::deserialize_extra_hosts",
		skip_serializing_if = "Vec::is_empty"
	)]
	pub extra_hosts: Vec<String>,
	/// `dns:` — custom DNS server IP(s) for name resolution; single value or list.
	#[serde(default)]
	pub dns: StringOrList,
	/// `dns_search:` — DNS search domains appended to unqualified names; single
	/// value or list.
	#[serde(default)]
	pub dns_search: StringOrList,
	/// `dns_opt:` — resolver options written to `/etc/resolv.conf` (e.g.
	/// `ndots:2`); single value or list.
	#[serde(default)]
	pub dns_opt: StringOrList,
	/// `network_mode:` — networking namespace mode (`bridge`, `host`, `none`,
	/// `service:NAME`, `container:NAME`); mutually exclusive with `networks:`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub network_mode: Option<String>,

	/// `depends_on:` — start/stop ordering and optional `condition:`
	/// (`service_started`/`service_healthy`/`service_completed_successfully`)
	/// dependencies on other services.
	#[serde(default)]
	pub depends_on: DependsOn,
	/// `healthcheck:` — command and timing that determine container health;
	/// `disable: true` turns off any image-defined check.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub healthcheck: Option<HealthCheck>,

	/// `restart:` — restart policy (`no`/`always`/`on-failure[:max]`/
	/// `unless-stopped`). Takes precedence over `deploy.restart_policy` when both
	/// are set.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub restart: Option<RestartPolicy>,
	/// `stop_signal:` — signal sent to stop the container (e.g. `SIGTERM`,
	/// `SIGINT`); default `SIGTERM`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub stop_signal: Option<String>,
	/// `stop_grace_period:` — duration string (e.g. `10s`, `1m30s`) to wait after
	/// `stop_signal` before `SIGKILL`; default `10s`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub stop_grace_period: Option<String>,
	/// `profiles:` — activation profiles; the service starts only when one of
	/// these profiles is enabled (no profiles = always enabled).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub profiles: Vec<String>,
	/// `post_start:` — lifecycle hook commands run inside the container right
	/// after it starts (Compose v2.30+).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub post_start: Vec<LifecycleHook>,
	/// `pre_stop:` — lifecycle hook commands run inside the container just before
	/// it is stopped (Compose v2.30+).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pre_stop: Vec<LifecycleHook>,

	/// `labels:` — metadata labels applied to the container; map or `key=value`
	/// list.
	#[serde(default)]
	pub labels: Labels,
	/// `annotations:` — OCI annotations on the container; distinct from `labels:`,
	/// passed through to the runtime.
	#[serde(default)]
	pub annotations: Labels,
	/// `container_name:` — explicit container name overriding the generated
	/// `<project>_<service>_<n>`; forbids scaling above 1 replica.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub container_name: Option<String>,
	/// `user:` — user (and optional `:group`) the process runs as, by name or
	/// numeric `UID[:GID]`; overrides the image `USER`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub user: Option<String>,
	/// `working_dir:` — working directory for the process; overrides the image
	/// `WORKDIR`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub working_dir: Option<String>,
	/// `group_add:` — additional groups (name or GID) the process joins, beyond
	/// its primary group.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub group_add: Vec<String>,
	/// `platform:` — target platform for the image (`os[/arch[/variant]]`, e.g.
	/// `linux/amd64`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub platform: Option<String>,

	/// `cap_add:` — Linux capabilities to grant on top of the runtime default set
	/// (e.g. `NET_ADMIN`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub cap_add: Vec<String>,
	/// `cap_drop:` — Linux capabilities to drop from the default set (`ALL` drops
	/// every capability).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub cap_drop: Vec<String>,
	/// `security_opt:` — runtime security options (e.g. `label:...`, `seccomp=...`,
	/// `no-new-privileges:true`, `apparmor=...`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub security_opt: Vec<String>,
	/// `read_only:` — when `true`, mounts the container's root filesystem
	/// read-only (writes only via volumes/tmpfs). Default `false`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub read_only: Option<bool>,
	/// `privileged:` — when `true`, grants extended host privileges; has reduced
	/// effect under rootless Podman. Default `false`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub privileged: Option<bool>,
	/// `init:` — when `true`, runs an init process (PID 1) that reaps zombies and
	/// forwards signals. Default `false`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub init: Option<bool>,
	/// `tty:` — when `true`, allocates a pseudo-TTY for the container (akin to
	/// `docker run -t`). Default `false`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub tty: Option<bool>,
	/// `stdin_open:` — when `true`, keeps stdin open for the container (akin to
	/// `docker run -i`). Default `false`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub stdin_open: Option<bool>,

	/// `runtime:` — OCI runtime to execute the container (e.g. `runc`, `crun`);
	/// default is the engine's configured runtime.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub runtime: Option<String>,
	/// `shm_size:` — size of `/dev/shm` as a size string (e.g. `512m`, `1g`);
	/// bytes if unitless.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub shm_size: Option<String>,
	/// `userns_mode:` — user-namespace mode (e.g. `host`, `keep-id`) controlling
	/// UID/GID mapping into the container.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub userns_mode: Option<String>,
	/// `pid:` — PID namespace to share (e.g. `host`, `container:NAME`,
	/// `service:NAME`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pid: Option<String>,
	/// `ipc:` — IPC namespace mode (e.g. `host`, `shareable`, `service:NAME`,
	/// `container:NAME`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub ipc: Option<String>,
	/// `uts:` — UTS namespace mode; `host` shares the host's hostname/domain
	/// namespace.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub uts: Option<String>,
	/// `cgroup_parent:` — optional parent cgroup under which the container's
	/// cgroup is created.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cgroup_parent: Option<String>,
	/// `cgroup:` — cgroup namespace mode (`host` or `private`) for the container.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cgroup: Option<String>,

	/// `devices:` — host devices exposed to the container
	/// (`host[:container[:perms]]`, perms a subset of `rwm`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub devices: Vec<String>,
	/// `device_cgroup_rules:` — explicit device cgroup allow/deny rules (e.g.
	/// `c 1:3 mr`).
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub device_cgroup_rules: Vec<String>,
	/// `storage_opt:` — per-container storage driver options (e.g. `size`) passed
	/// to the graph driver.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub storage_opt: HashMap<String, String>,

	/// `scale:` — number of replica containers to run. Takes precedence over
	/// `deploy.replicas`; a CLI `--scale` override wins over both.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub scale: Option<u32>,
	/// `cpu_shares:` — relative CPU weight under contention (default `1024`);
	/// proportional share, not a hard cap like `cpus:`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu_shares: Option<u64>,
	/// `cpu_quota:` — CFS hard cap in microseconds of CPU time per `cpu_period`;
	/// `cpus:` is the higher-level equivalent.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu_quota: Option<i64>,
	/// `cpu_period:` — CFS scheduler period in microseconds against which
	/// `cpu_quota` is measured (default `100000`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu_period: Option<u64>,
	/// `cpuset:` — explicit CPUs/cores the container may run on (e.g. `0-3`,
	/// `0,2`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpuset: Option<String>,
	/// `cpus:` — fractional number of CPUs to allow (e.g. `0.5`); a hard cap
	/// converted to a CFS quota. Top-level value wins over `deploy.resources`.
	/// Accepts both the unquoted number (`cpus: 0.5`) and the quoted string form.
	#[serde(
		default,
		deserialize_with = "super::primitives::deserialize_opt_string_or_number",
		skip_serializing_if = "Option::is_none"
	)]
	pub cpus: Option<String>,
	/// `cpu_count:` — number of usable CPUs (Windows containers); not honored on
	/// Linux/Podman.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu_count: Option<i64>,
	/// `cpu_percent:` — usable percentage of available CPU (Windows containers);
	/// not honored on Linux/Podman.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu_percent: Option<i64>,
	/// `cpu_rt_runtime:` — real-time CPU runtime per period in microseconds for
	/// real-time scheduling.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu_rt_runtime: Option<i64>,
	/// `cpu_rt_period:` — real-time scheduler period in microseconds for
	/// real-time scheduling.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub cpu_rt_period: Option<i64>,
	/// `mem_limit:` — hard memory cap as a size string (e.g. `512m`, `1g`); bytes
	/// if unitless. Top-level value wins over `deploy.resources.limits.memory`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mem_limit: Option<String>,
	/// `memswap_limit:` — total memory + swap limit as a size string; `-1` allows
	/// unlimited swap.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub memswap_limit: Option<String>,
	/// `mem_reservation:` — soft memory limit (size string, e.g. `256m`) enforced
	/// under host memory pressure; must be below `mem_limit`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mem_reservation: Option<String>,
	/// `mem_swappiness:` — swap tendency for the container's memory, `0`–`100`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub mem_swappiness: Option<i64>,
	/// `pids_limit:` — maximum number of PIDs the container may create; `-1` for
	/// unlimited. Top-level value wins over `deploy.resources.limits.pids`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pids_limit: Option<i64>,
	/// `oom_kill_disable:` — when `true`, disables the OOM killer for the
	/// container; not supported on cgroups v2.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub oom_kill_disable: Option<bool>,
	/// `oom_score_adj:` — OOM-killer preference (`-1000`..`1000`); higher makes
	/// the container likelier to be killed first.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub oom_score_adj: Option<i64>,
	/// `blkio_config:` — block-IO weights and per-device read/write bandwidth and
	/// IOPS limits.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub blkio_config: Option<BlkioConfig>,

	/// `logging:` — log driver and its options for the container's stdout/stderr.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub logging: Option<LoggingConfig>,
	/// `sysctls:` — kernel parameters set in the container's namespace (e.g.
	/// `net.core.somaxconn`); map or list form.
	#[serde(default)]
	pub sysctls: Sysctls,
	/// `ulimits:` — per-resource process limits (e.g. `nofile`) as a single
	/// value or `{soft, hard}` pair, keyed by limit name.
	#[serde(default, skip_serializing_if = "IndexMap::is_empty")]
	pub ulimits: IndexMap<String, UlimitConfig>,

	/// `label_file:` — file(s) of `key=value` lines loaded as container labels;
	/// single path or list (Compose v2.32+).
	#[serde(default)]
	pub label_file: StringOrList,

	/// `attach:` — when `false`, `up` does not stream this service's logs to the
	/// console. Default `true`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub attach: Option<bool>,

	/// `pull_policy:` — when to pull the image: `always`, `never`, `missing` (the
	/// default; alias `if_not_present`), `build`, or `newer`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub pull_policy: Option<String>,

	/// `deploy:` — deployment config (replicas, resource limits/reservations,
	/// restart policy); a fallback for top-level `scale`/`cpus`/`mem_limit`/
	/// `restart` and the source of Swarm-only fields.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub deploy: Option<DeployConfig>,

	/// `develop:` — `watch` rules driving file-sync/rebuild during development.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub develop: Option<DevelopConfig>,

	/// `gpus:` — GPU devices to expose (`all` or a capability/count spec),
	/// forwarded as CDI device reservations.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub gpus: Option<GpuSpec>,

	/// `credential_spec:` — Windows managed-service-account credential source
	/// (`config`/`file`/`registry`). Parsed for fidelity; podup has no rootless
	/// Podman equivalent, so the diagnostics pass reports it as not honored.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub credential_spec: Option<CredentialSpec>,

	/// `isolation:` — container isolation technology (e.g. `default`, `process`,
	/// `hyperv`). Distinct from `build.isolation`. Parsed for fidelity; podup has
	/// no rootless Podman equivalent, so it is reported as not honored.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub isolation: Option<String>,

	/// `provider:` (Compose v2.36) — delegates the service lifecycle to an
	/// external provider plugin. Parsed for fidelity; podup invokes no provider
	/// plugins, so it is reported as not honored.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub provider: Option<ProviderConfig>,

	/// `use_api_socket:` (Compose v2.37.1) — bind-mount the Docker API socket and
	/// forward credentials into the container. Parsed for fidelity; podup has no
	/// equivalent, so it is reported as not honored.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub use_api_socket: Option<bool>,

	/// Keys present in the YAML that don't map to a known service field. Captured
	/// (rather than silently dropped) so the parser can warn about likely typos
	/// while still tolerating compose-spec `x-*` extensions and forward-compatible
	/// keys. Not part of the container spec; round-tripped by the `config`
	/// subcommand to preserve fidelity.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub unknown: IndexMap<String, serde_yaml::Value>,
}

/// `x-podman-autoupdate` extension key, Podman's auto-update policy for the
/// container. Mirrors the `x-podman-on-failure` shape: a Compose-spec
/// `x-`-prefixed key, parsed from the service's captured `unknown` map so the
/// compose file stays portable to docker compose.
pub const X_PODMAN_AUTOUPDATE: &str = "x-podman-autoupdate";

/// What Podman's `auto-update` does with a container carrying the
/// `io.containers.autoupdate` label.
///
/// `registry`: on every `up`, pull the image and recreate the container if the
/// pulled image moved the tag. `local`: do not check the registry, a locally
/// rebuilt image that moves the tag is still caught by the existing config-hash
/// + image-ID comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoUpdate {
	/// Check the registry on every `up` and recreate on a tag move.
	Registry,
	/// Stay with the local image; rely on the existing image-ID comparison for
	/// locally rebuilt images.
	Local,
}

impl std::str::FromStr for AutoUpdate {
	type Err = String;

	fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
		match value {
			"registry" => Ok(Self::Registry),
			"local" => Ok(Self::Local),
			other => Err(format!(
				"invalid {X_PODMAN_AUTOUPDATE} value {other:?} (expected one of: registry, local)"
			)),
		}
	}
}

impl AutoUpdate {
	/// The Quadlet `AutoUpdate=` value and the `io.containers.autoupdate` label
	/// value, the same spelling Podman expects in both places.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Registry => "registry",
			Self::Local => "local",
		}
	}
}

impl Service {
	/// The `x-podman-autoupdate` extension, parsed and validated.
	///
	/// Spec-defined keys are typed fields on this struct; a Podman extension
	/// lives in `unknown` (where `#[serde(flatten)]` already preserves it for
	/// round-tripping) and is read through here. `Err` for a value that is not
	/// one of Podman's two policies, so a typo is rejected rather than silently
	/// doing nothing.
	pub fn podman_autoupdate(&self) -> std::result::Result<Option<AutoUpdate>, String> {
		let Some(raw) = self.unknown.get(X_PODMAN_AUTOUPDATE) else {
			return Ok(None);
		};
		let Some(text) = raw.as_str() else {
			return Err(format!(
				"{X_PODMAN_AUTOUPDATE} must be a string (expected one of: registry, local)"
			));
		};
		text.parse().map(Some)
	}
}

/// `credential_spec:` — source of a Windows managed-service-account credential
/// spec. Exactly one of `config`/`file`/`registry` is expected per the Compose
/// Spec; podup parses all three for fidelity but honors none.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct CredentialSpec {
	/// `config:` — ID of a Compose config object holding the credential spec.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub config: Option<String>,
	/// `file:` — path to a credential-spec file, relative to the Docker data
	/// directory.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub file: Option<String>,
	/// `registry:` — Windows registry value name holding the credential spec.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub registry: Option<String>,
	/// Forward-compatible keys captured so a typo is surfaced rather than dropped.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub unknown: IndexMap<String, serde_yaml::Value>,
}

/// `provider:` (Compose v2.36) — names an external provider plugin (`type`) and
/// its free-form `options`. Parsed for fidelity; podup invokes no providers.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct ProviderConfig {
	/// `type:` — name of the external provider plugin to delegate the service
	/// lifecycle to.
	#[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
	pub provider_type: Option<String>,
	/// `options:` — free-form provider-specific parameters passed through to the
	/// plugin.
	#[serde(default, skip_serializing_if = "IndexMap::is_empty")]
	pub options: IndexMap<String, serde_yaml::Value>,
	/// Forward-compatible keys captured so a typo is surfaced rather than dropped.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub unknown: IndexMap<String, serde_yaml::Value>,
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
