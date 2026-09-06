//! Field-by-field merge of a base service into an overriding service.
//!
//! Scalar fields take the override when present, else the base. Collection
//! fields (env vars, labels, maps) are merged with the override winning on
//! overlapping keys. Sequence fields are combined per the Compose
//! Specification's `extends` rules: referenced (base) items first, then the
//! extending service's items, with duplicates removed, not replaced wholesale.

use super::super::types::{
	DependsOn, DependsOnCondition, EnvFile, EnvVars, Labels, Service, ServiceCondition,
	ServiceNetworks, StringOrList, Sysctls,
};

/// [`merge_service`] with the overriding file's `!override`/`!reset` tags.
///
/// A key tagged `!override` takes the overriding value whole, skipping whatever
/// combine rule would normally apply; `!reset` drops the key entirely, leaving
/// the type's default. Both were accepted and silently ignored before, so a file
/// asking for replacement got an append instead, the opposite of what it said.
///
/// Implemented by pre-shaping the two sides and then running the ordinary merge,
/// rather than by branching inside it: `!reset` empties both sides so the result
/// is the default, and `!override` empties only the base so the combine rule has
/// nothing to combine with and the overriding value survives whole. One rule
/// each, and the field-by-field merge below stays the single place that knows
/// how any given key combines.
pub(in crate::compose) fn merge_service_tagged(
	base: Service,
	override_svc: Service,
	tagged: Option<&std::collections::HashMap<String, crate::compose::tags::MergeTag>>,
) -> Service {
	let Some(tagged) = tagged.filter(|t| !t.is_empty()) else {
		return merge_service(base, override_svc);
	};
	let mut base = base;
	let mut over = override_svc;
	for (key, tag) in tagged {
		clear_service_key(&mut base, key);
		if matches!(tag, crate::compose::tags::MergeTag::Reset) {
			clear_service_key(&mut over, key);
		}
	}
	merge_service(base, over)
}

/// Reset one service key to its default, by name: the primitive both tags are
/// built from.
///
/// A key podup does not model is ignored: a tag cannot change a merge that never
/// happens, and refusing it would reject a file that is valid elsewhere.
fn clear_service_key(svc: &mut Service, key: &str) {
	match key {
		"ports" => svc.ports = Vec::new(),
		"expose" => svc.expose = Vec::new(),
		"volumes" => svc.volumes = Vec::new(),
		"volumes_from" => svc.volumes_from = Vec::new(),
		"networks" => svc.networks = Default::default(),
		"environment" => svc.environment = Default::default(),
		"env_file" => svc.env_file = Default::default(),
		"labels" => svc.labels = Default::default(),
		"label_file" => svc.label_file = Default::default(),
		"dns" => svc.dns = Default::default(),
		"dns_search" => svc.dns_search = Default::default(),
		"dns_opt" => svc.dns_opt = Default::default(),
		"tmpfs" => svc.tmpfs = Default::default(),
		"sysctls" => svc.sysctls = Default::default(),
		"cap_add" => svc.cap_add = Vec::new(),
		"cap_drop" => svc.cap_drop = Vec::new(),
		"devices" => svc.devices = Vec::new(),
		"extra_hosts" => svc.extra_hosts = Vec::new(),
		"ulimits" => svc.ulimits = Default::default(),
		"depends_on" => svc.depends_on = Default::default(),
		"secrets" => svc.secrets = Vec::new(),
		"configs" => svc.configs = Vec::new(),
		"links" => svc.links = Vec::new(),
		"external_links" => svc.external_links = Vec::new(),
		"group_add" => svc.group_add = Vec::new(),
		"security_opt" => svc.security_opt = Vec::new(),
		"profiles" => svc.profiles = Vec::new(),
		_ => {}
	}
}

pub(in crate::compose) fn merge_service(base: Service, override_svc: Service) -> Service {
	fn opt<T>(o: Option<T>, b: Option<T>) -> Option<T> {
		o.or(b)
	}

	fn merge_envvars(base: EnvVars, over: EnvVars) -> EnvVars {
		if matches!(over, EnvVars::Empty) && !matches!(base, EnvVars::Empty) {
			return base;
		}
		if matches!(base, EnvVars::Empty) {
			return over;
		}
		let mut merged: indexmap::IndexMap<String, Option<serde_yaml::Value>> =
			indexmap::IndexMap::new();
		for (k, v) in base.to_map() {
			merged.insert(k, v.map(serde_yaml::Value::String));
		}
		for (k, v) in over.to_map() {
			merged.insert(k, v.map(serde_yaml::Value::String));
		}
		EnvVars::Map(merged)
	}

	fn merge_labels(base: Labels, over: Labels) -> Labels {
		if base.is_empty() && over.is_empty() {
			return Labels::Empty;
		}
		let mut map: indexmap::IndexMap<String, String> = indexmap::IndexMap::new();
		for (k, v) in base.to_map() {
			map.insert(k, v);
		}
		for (k, v) in over.to_map() {
			map.insert(k, v);
		}
		Labels::Map(map)
	}

	fn merge_vec<T: Clone + serde::Serialize>(base: Vec<T>, over: Vec<T>) -> Vec<T> {
		// Compose `extends` combines sequences: base items first, then the
		// extending service's items, dropping exact duplicates. Equality is by
		// serialized form so the element types need not implement PartialEq.
		let mut seen: Vec<String> = base
			.iter()
			.filter_map(|item| serde_yaml::to_string(item).ok())
			.collect();
		let mut out = base;
		for item in over {
			match serde_yaml::to_string(&item) {
				Ok(key) if seen.contains(&key) => continue,
				Ok(key) => seen.push(key),
				Err(_) => {}
			}
			out.push(item);
		}
		out
	}

	/// Append, do not replace. docker compose concatenates these sequences
	/// across `-f` and `extends`; replacing silently dropped the base's entries,
	/// so an override adding one nameserver removed every other one.
	fn merge_sol(base: StringOrList, over: StringOrList) -> StringOrList {
		if over.is_empty() {
			return base;
		}
		if base.is_empty() {
			return over;
		}
		let mut out = base.to_list();
		for item in over.to_list() {
			if !out.contains(&item) {
				out.push(item);
			}
		}
		StringOrList::List(out)
	}

	/// Append, do not replace: docker compose reads the base's env files *and*
	/// the override's, in order. Replacing meant an override adding one file
	/// silently stopped loading every other one.
	fn merge_env_file(base: EnvFile, over: EnvFile) -> EnvFile {
		if over.is_empty() {
			return base;
		}
		if base.is_empty() {
			return over;
		}
		let mut out = base.to_entries();
		for entry in over.to_entries() {
			if !out.iter().any(|e| e.path() == entry.path()) {
				out.push(entry);
			}
		}
		EnvFile::List(out)
	}

	// compose-go unions `depends_on` across `extends`: the base's dependencies and
	// the extending service's are both kept, the override winning per service key
	// (same rule as `environment`). A bare-list entry canonicalizes to the default
	// `service_started` condition, which matches `DependsOn`'s list-form defaults.
	fn merge_depends_on(base: DependsOn, over: DependsOn) -> DependsOn {
		if matches!(over, DependsOn::Empty) {
			return base;
		}
		if matches!(base, DependsOn::Empty) {
			return over;
		}
		fn to_map(d: DependsOn) -> indexmap::IndexMap<String, DependsOnCondition> {
			match d {
				DependsOn::Empty => indexmap::IndexMap::new(),
				DependsOn::List(v) => v
					.into_iter()
					.map(|name| {
						(
							name,
							DependsOnCondition {
								condition: ServiceCondition::ServiceStarted,
								restart: None,
								required: None,
							},
						)
					})
					.collect(),
				DependsOn::Map(m) => m,
			}
		}
		let mut merged = to_map(base);
		for (k, v) in to_map(over) {
			merged.insert(k, v);
		}
		DependsOn::Map(merged)
	}

	Service {
		image: opt(override_svc.image, base.image),
		build: override_svc.build.or(base.build),
		extends: override_svc.extends.or(base.extends),
		command: override_svc.command.or(base.command),
		entrypoint: override_svc.entrypoint.or(base.entrypoint),
		ports: merge_vec(base.ports, override_svc.ports),
		expose: merge_vec(base.expose, override_svc.expose),
		environment: merge_envvars(base.environment, override_svc.environment),
		env_file: merge_env_file(base.env_file, override_svc.env_file),
		volumes: merge_volumes(base.volumes, override_svc.volumes),
		tmpfs: merge_sol(base.tmpfs, override_svc.tmpfs),
		volumes_from: merge_vec(base.volumes_from, override_svc.volumes_from),
		configs: merge_vec(base.configs, override_svc.configs),
		secrets: merge_vec(base.secrets, override_svc.secrets),
		// Union, not replace. A service on `backend` in the base and `monitoring`
		// in the override silently lost `backend`, dropped off the network, and
		// service discovery failed at run time, far from the config that caused
		// it. docker compose unions them; the override's per-network config wins
		// for a network both declare.
		networks: merge_networks(base.networks, override_svc.networks),
		hostname: override_svc.hostname.or(base.hostname),
		domainname: override_svc.domainname.or(base.domainname),
		mac_address: override_svc.mac_address.or(base.mac_address),
		links: merge_vec(base.links, override_svc.links),
		external_links: merge_vec(base.external_links, override_svc.external_links),
		extra_hosts: merge_vec(base.extra_hosts, override_svc.extra_hosts),
		dns: merge_sol(base.dns, override_svc.dns),
		dns_search: merge_sol(base.dns_search, override_svc.dns_search),
		dns_opt: merge_sol(base.dns_opt, override_svc.dns_opt),
		network_mode: override_svc.network_mode.or(base.network_mode),
		depends_on: merge_depends_on(base.depends_on, override_svc.depends_on),
		healthcheck: override_svc.healthcheck.or(base.healthcheck),
		restart: override_svc.restart.or(base.restart),
		stop_signal: override_svc.stop_signal.or(base.stop_signal),
		stop_grace_period: override_svc.stop_grace_period.or(base.stop_grace_period),
		profiles: merge_vec(base.profiles, override_svc.profiles),
		post_start: merge_vec(base.post_start, override_svc.post_start),
		pre_stop: merge_vec(base.pre_stop, override_svc.pre_stop),
		labels: merge_labels(base.labels, override_svc.labels),
		annotations: merge_labels(base.annotations, override_svc.annotations),
		container_name: override_svc.container_name.or(base.container_name),
		user: override_svc.user.or(base.user),
		working_dir: override_svc.working_dir.or(base.working_dir),
		group_add: merge_vec(base.group_add, override_svc.group_add),
		platform: override_svc.platform.or(base.platform),
		cap_add: merge_vec(base.cap_add, override_svc.cap_add),
		cap_drop: merge_vec(base.cap_drop, override_svc.cap_drop),
		security_opt: merge_vec(base.security_opt, override_svc.security_opt),
		read_only: override_svc.read_only.or(base.read_only),
		privileged: override_svc.privileged.or(base.privileged),
		init: override_svc.init.or(base.init),
		tty: override_svc.tty.or(base.tty),
		stdin_open: override_svc.stdin_open.or(base.stdin_open),
		runtime: override_svc.runtime.or(base.runtime),
		shm_size: override_svc.shm_size.or(base.shm_size),
		userns_mode: override_svc.userns_mode.or(base.userns_mode),
		pid: override_svc.pid.or(base.pid),
		ipc: override_svc.ipc.or(base.ipc),
		uts: override_svc.uts.or(base.uts),
		cgroup_parent: override_svc.cgroup_parent.or(base.cgroup_parent),
		cgroup: override_svc.cgroup.or(base.cgroup),
		devices: merge_vec(base.devices, override_svc.devices),
		device_cgroup_rules: merge_vec(base.device_cgroup_rules, override_svc.device_cgroup_rules),
		storage_opt: {
			let mut m = base.storage_opt;
			for (k, v) in override_svc.storage_opt {
				m.insert(k, v);
			}
			m
		},
		scale: override_svc.scale.or(base.scale),
		cpu_shares: override_svc.cpu_shares.or(base.cpu_shares),
		cpu_quota: override_svc.cpu_quota.or(base.cpu_quota),
		cpu_period: override_svc.cpu_period.or(base.cpu_period),
		cpuset: override_svc.cpuset.or(base.cpuset),
		cpus: override_svc.cpus.or(base.cpus),
		cpu_count: override_svc.cpu_count.or(base.cpu_count),
		cpu_percent: override_svc.cpu_percent.or(base.cpu_percent),
		cpu_rt_runtime: override_svc.cpu_rt_runtime.or(base.cpu_rt_runtime),
		cpu_rt_period: override_svc.cpu_rt_period.or(base.cpu_rt_period),
		mem_limit: override_svc.mem_limit.or(base.mem_limit),
		memswap_limit: override_svc.memswap_limit.or(base.memswap_limit),
		mem_reservation: override_svc.mem_reservation.or(base.mem_reservation),
		mem_swappiness: override_svc.mem_swappiness.or(base.mem_swappiness),
		pids_limit: override_svc.pids_limit.or(base.pids_limit),
		oom_kill_disable: override_svc.oom_kill_disable.or(base.oom_kill_disable),
		oom_score_adj: override_svc.oom_score_adj.or(base.oom_score_adj),
		blkio_config: override_svc.blkio_config.or(base.blkio_config),
		logging: override_svc.logging.or(base.logging),
		// Merged per key, like `environment` and `labels`, not replaced.
		sysctls: merge_sysctls(base.sysctls, override_svc.sysctls),
		ulimits: {
			let mut m = base.ulimits;
			for (k, v) in override_svc.ulimits {
				m.insert(k, v);
			}
			m
		},
		label_file: merge_sol(base.label_file, override_svc.label_file),
		attach: override_svc.attach.or(base.attach),
		pull_policy: override_svc.pull_policy.or(base.pull_policy),
		deploy: override_svc.deploy.or(base.deploy),
		develop: override_svc.develop.or(base.develop),
		gpus: override_svc.gpus.or(base.gpus),
		credential_spec: override_svc.credential_spec.or(base.credential_spec),
		isolation: override_svc.isolation.or(base.isolation),
		provider: override_svc.provider.or(base.provider),
		use_api_socket: override_svc.use_api_socket.or(base.use_api_socket),
		unknown: {
			// Keep unknown keys from both sides so a typo in either the base or
			// the overriding service is still surfaced; the override wins on
			// conflicting keys.
			let mut u = base.unknown;
			u.extend(override_svc.unknown);
			u
		},
	}
}

/// Merge `volumes:` by **container-side target**, with the override winning.
///
/// `merge_vec` dedups by serialized form, so a base mounting `./a:/data` and an
/// override remapping it to `./b:/data` produced both entries, and
/// `podman create` refused the container with `duplicate mount destination`.
/// docker compose replaces the mount at a target it already has, which is what
/// remapping a path in an override is *for*.
///
/// Base order is preserved so an unchanged mount stays where it was; an override
/// introducing a new target appends.
fn merge_volumes(
	base: Vec<crate::compose::types::VolumeMount>,
	over: Vec<crate::compose::types::VolumeMount>,
) -> Vec<crate::compose::types::VolumeMount> {
	let mut out = base;
	for item in over {
		match out.iter().position(|m| m.target() == item.target()) {
			Some(i) => out[i] = item,
			None => out.push(item),
		}
	}
	out
}

/// Union two `networks:` declarations, keeping the base's attachments and adding
/// the override's. For a network both declare, the override's per-network config
/// wins; a bare-list entry contributes no config, so it never erases one.
///
/// The short (list) and long (map) forms mix freely across `-f` and `extends`,
/// so both collapse to the map form whenever either side carries config.
fn merge_networks(base: ServiceNetworks, over: ServiceNetworks) -> ServiceNetworks {
	if matches!(over, ServiceNetworks::Empty) {
		return base;
	}
	if matches!(base, ServiceNetworks::Empty) {
		return over;
	}
	// Preserve declaration order: the base's networks first, then any the
	// override introduces.
	let mut out: indexmap::IndexMap<String, Option<crate::compose::types::ServiceNetworkConfig>> =
		indexmap::IndexMap::new();
	for name in base.names() {
		let cfg = base.config_for(&name).cloned();
		out.insert(name, cfg);
	}
	for name in over.names() {
		let cfg = over.config_for(&name).cloned();
		match out.entry(name) {
			indexmap::map::Entry::Occupied(mut e) => {
				// A bare name in the override must not wipe config the base set.
				if cfg.is_some() {
					e.insert(cfg);
				}
			}
			indexmap::map::Entry::Vacant(e) => {
				e.insert(cfg);
			}
		}
	}
	// Stay in the short form when nothing carries per-network config. That is the
	// form the user wrote and the one docker keeps, and a map of all-`None`
	// values is pruned as empty by the `config` renderer's null-stripping, which
	// would drop the networks from the rendered file entirely.
	if out.values().all(Option::is_none) {
		return ServiceNetworks::List(out.into_keys().collect());
	}
	ServiceNetworks::Map(out)
}

/// Merge `sysctls:` per key, the way `environment` and `labels` already merge:
/// the override wins for a key both set, and the base keeps the rest.
fn merge_sysctls(base: Sysctls, over: Sysctls) -> Sysctls {
	if matches!(over, Sysctls::Empty) {
		return base;
	}
	if matches!(base, Sysctls::Empty) {
		return over;
	}
	let mut out: indexmap::IndexMap<String, serde_yaml::Value> = indexmap::IndexMap::new();
	for (k, v) in base.to_map() {
		out.insert(k, serde_yaml::Value::String(v));
	}
	for (k, v) in over.to_map() {
		out.insert(k, serde_yaml::Value::String(v));
	}
	Sysctls::Map(out)
}

#[cfg(test)]
mod tests;
