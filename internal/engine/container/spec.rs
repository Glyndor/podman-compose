//! Build the `SpecGenerator` literal the container-create path sends to
//! `POST /libpod/containers/create`.
//!
//! Split out of [`super::mod`] so the orchestrator in `mod.rs` stays a
//! readable sequence of "compute input, hand it to the builder" steps
//! while this file owns every field's wire-level mapping. The
//! `cargo mutants` validation lives in `spec_body_tests.rs` (one assertion
//! per field); `mod.rs` is for orchestration, this file is for the spec.

use std::collections::HashMap;

use crate::compose::types::Service;
use crate::error::Result;
use crate::libpod::types::container::{
	LinuxDevice, LinuxDeviceCgroup, LinuxResources, Mount, NamedVolume, Namespace,
	PerNetworkOptions, PortMapping, Secret, SpecGenerator, Ulimit,
};

/// Every computed input the `SpecGenerator` literal needs, gathered into
/// one struct so the builder's signature stays readable. The orchestrator
/// in `super::mod` does the compute work, then hands the bundle to
/// [`build_spec_generator`].
pub(crate) struct SpecInputs {
	/// Whether the project runs in pod mode (`x-podman-pod: true`). In pod
	/// mode the spec gets `pod` set, `portmappings`/`networks`/`netns` are
	/// forced empty.
	pub in_pod: bool,
	/// Resolved container name.
	pub container_name: String,
	/// Image reference (the service's `image:`, or the build-only tag).
	pub image: String,
	/// Parsed `ports:` list, in spec-generator form (empty in pod mode).
	pub portmappings: Vec<PortMapping>,
	/// Resolved `expose:` map (port -> protocol).
	pub expose: HashMap<u16, String>,
	/// Resolved `networks:` map (empty in pod mode).
	pub networks: HashMap<String, PerNetworkOptions>,
	/// Resolved `netns` (`None` in pod mode; otherwise `bridge` when networks
	/// are set or the explicit `network_mode` value).
	pub netns: Option<Namespace>,
	/// All computed labels, including the `podup.*` ones.
	pub labels: HashMap<String, String>,
	/// OCI annotations from the service's `annotations:` map.
	pub annotations: HashMap<String, String>,
	/// Resolved sysctls (`sysctls:`).
	pub sysctl: HashMap<String, String>,
	/// Resource limits (`mem_limit`, `cpus`, `deploy.resources.limits`, …).
	pub resource_limits: Option<LinuxResources>,
	/// Ulimits (POSIX rlimits), renamed to `r_limits` on the wire.
	pub ulimits: Vec<Ulimit>,
	/// OCI mounts (bind/tmpfs/...).
	pub mounts: Vec<Mount>,
	/// Named-volume attachments (resolved to the project-prefixed names).
	pub named_volumes: Vec<NamedVolume>,
	/// Container references a service inherits volumes from.
	pub volumes_from: Vec<String>,
	/// Podman-native secret references.
	pub native_secrets: Vec<Secret>,
	/// Devices to expose (raw device paths + CDI device names).
	pub devices: Vec<LinuxDevice>,
	/// Device cgroup rules (structured).
	pub device_cgroup_rule: Vec<LinuxDeviceCgroup>,
	/// Security options decomposed into dedicated fields.
	pub security: SecurityInputs,
	/// Namespace modes (one per namespace).
	pub namespaces: NamespaceInputs,
	/// Restart policy name + max attempts.
	pub restart: (Option<String>, Option<u64>),
	/// Resolved stop signal (numeric) and timeout.
	pub stop_signal_timeout: (Option<i64>, Option<u64>),
	/// Service command/entrypoint as exec arrays.
	pub command: Option<Vec<String>>,
	pub entrypoint: Option<Vec<String>>,
	/// Environment as a `HashMap<key, value>` (already filtered for bare
	/// `KEY` passthrough semantics).
	pub env: HashMap<String, String>,
	/// Service-level links resolved to `container:alias` strings.
	pub links: Vec<String>,
	/// Image OS / arch hint, derived from the service's `platform:`.
	pub image_platform: (Option<String>, Option<String>),
	/// Storage driver options (`storage_opt:`).
	pub storage_opts: HashMap<String, String>,
}

/// Security-related fields the spec builder needs. Carved out of
/// [`SpecInputs`] so the call site reads cleanly and the security module
/// owns the parse.
pub(crate) struct SecurityInputs {
	pub selinux_opts: Vec<String>,
	pub apparmor_profile: Option<String>,
	pub seccomp_profile_path: Option<String>,
	pub no_new_privileges: Option<bool>,
	pub mask: Vec<String>,
	pub unmask: Vec<String>,
}

/// Namespace modes, one per namespace. `None` means "leave as default".
pub(crate) struct NamespaceInputs {
	pub userns: Option<Namespace>,
	pub pidns: Option<Namespace>,
	pub ipcns: Option<Namespace>,
	pub utsns: Option<Namespace>,
	pub cgroupns: Option<Namespace>,
}

/// Build the `SpecGenerator` literal the engine sends to
/// `POST /libpod/containers/create`. Pure over its inputs: no HTTP, no
/// logging, no Podman client. The orchestrator in `super::mod` is
/// responsible for the warn-then-call ordering.
#[allow(clippy::too_many_arguments, dead_code)]
pub(crate) fn build_spec_generator(
	project: &str,
	service: &Service,
	healthconfig: Option<crate::libpod::types::container::HealthConfig>,
	log_configuration: Option<crate::libpod::types::container::LogConfig>,
	health_check_on_failure_action: Option<
		crate::libpod::types::container::HealthCheckOnFailureAction,
	>,
	inputs: SpecInputs,
) -> Result<SpecGenerator> {
	let SpecInputs {
		in_pod,
		container_name,
		image,
		portmappings,
		expose,
		networks,
		netns,
		labels,
		annotations,
		sysctl,
		resource_limits,
		ulimits,
		mounts,
		named_volumes,
		volumes_from,
		native_secrets,
		devices,
		device_cgroup_rule,
		security: sec,
		namespaces,
		restart: (restart_policy, restart_tries),
		stop_signal_timeout: (stop_signal, stop_timeout),
		command,
		entrypoint,
		env,
		links,
		image_platform: (image_os, image_arch),
		storage_opts,
	} = inputs;

	Ok(SpecGenerator {
		name: container_name,
		image,
		pod: if in_pod {
			Some(project.to_string())
		} else {
			None
		},
		command,
		entrypoint,
		env,
		terminal: service.tty,
		stdin: service.stdin_open,
		user: service.user.clone(),
		work_dir: service.working_dir.clone(),
		stop_signal,
		stop_timeout,
		hostname: service.hostname.clone(),
		domainname: service.domainname.clone(),
		labels,
		annotations,
		cap_add: service.cap_add.clone(),
		cap_drop: service.cap_drop.clone(),
		privileged: service.privileged,
		read_only_filesystem: service.read_only,
		selinux_opts: sec.selinux_opts,
		apparmor_profile: sec.apparmor_profile,
		seccomp_profile_path: sec.seccomp_profile_path,
		no_new_privileges: sec.no_new_privileges,
		mask: sec.mask,
		unmask: sec.unmask,
		sysctl,
		expose,
		portmappings,
		networks,
		netns,
		extra_hosts: service.extra_hosts.clone(),
		dns_server: service.dns.to_list(),
		dns_search: service.dns_search.to_list(),
		dns_option: service.dns_opt.to_list(),
		mounts,
		volumes: named_volumes,
		volumes_from,
		secrets: native_secrets,
		userns: namespaces.userns,
		pidns: namespaces.pidns,
		ipcns: namespaces.ipcns,
		utsns: namespaces.utsns,
		cgroupns: namespaces.cgroupns,
		cgroup_parent: service.cgroup_parent.clone(),
		resource_limits,
		ulimits,
		shm_size: service
			.shm_size
			.as_deref()
			.and_then(crate::size::parse_memory),
		healthconfig,
		health_check_on_failure_action,
		log_configuration,
		init: service.init,
		restart_policy,
		restart_tries,
		devices,
		device_cgroup_rule,
		groups: service.group_add.clone(),
		oom_score_adj: service.oom_score_adj,
		runtime: service.runtime.clone(),
		links,
		image_os,
		image_arch,
		storage_opts,
		..Default::default()
	})
}
