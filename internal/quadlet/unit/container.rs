//! Build the `.container` unit for a service.

use indexmap::IndexMap;

use crate::compose::types::{RestartPolicy, SecretConfig, Service};
use crate::engine::build_log_config;
use crate::ports::parse_ports;
use crate::size::parse_duration_secs;

use super::health::render_healthcheck;
use super::security::{is_inline_secret, map_security_opt, render_secret};
use super::{
	abs_against, collect_warnings, owner_marker, render_command, render_publish_port,
	render_restart, render_tmpfs_mount, render_volume, sorted_label_pairs, sorted_pairs, unit_stem,
	QuadletUnit, Section,
};
use crate::quadlet::is_no_warn_set;

/// Project-wide inputs every generated unit needs, as opposed to the per-service
/// ones (`name`, `service`). Grouped rather than passed loose because the set
/// only grows as more compose keys gain a Quadlet mapping, and because they are
/// identical for every service in one `generate_at` call.
pub(crate) struct UnitContext<'a> {
	/// Compose project name, stamped as the `podup.project` ownership label.
	pub project: &'a str,
	/// Volumes the compose file declares (external ones excluded).
	pub declared_volumes: &'a [&'a str],
	/// Networks the compose file declares (external ones excluded).
	pub declared_networks: &'a [&'a str],
	/// Top-level `secrets:` definitions, for resolving a service's secret refs.
	pub secrets: &'a IndexMap<String, SecretConfig>,
	/// Directory compose resolves relative paths against — the compose file's
	/// own directory, not the unit's. See [`abs_against`].
	pub base_dir: &'a std::path::Path,
}

/// Build the `.container` unit for one compose `service`.
///
/// The project name is stamped onto the unit as the `podup.project` ownership
/// label (and the service key as `podup.service`), matching the labels the live
/// engine applies, so generated containers are traceable back to their project
/// the same way running ones are.
pub(crate) fn container_unit(
	name: &str,
	service: &Service,
	ctx: &UnitContext<'_>,
	warnings: &mut Vec<String>,
) -> QuadletUnit {
	let UnitContext {
		project,
		declared_volumes,
		declared_networks,
		secrets,
		base_dir,
	} = ctx;
	let mut unit = Section::new("Unit");
	unit.add("Description", format!("{name} (podup)"));
	for dep in service.depends_on.service_names() {
		// The dependency's generated unit is named `{unit_stem(project, dep)}.container`,
		// so its service is `{unit_stem(project, dep)}.service`; reference that, not the
		// raw compose key, or the ordering would target a non-existent unit.
		let dep_service = format!("{}.service", unit_stem(project, &dep));
		unit.add("After", dep_service.clone());
		if service.depends_on.required_for(&dep) {
			unit.add("Requires", dep_service);
		} else {
			unit.add("Wants", dep_service);
		}
	}

	let mut container = Section::new("Container");
	// Default the container name to `{project}-{service}`, matching how `up`
	// names containers. Without the project prefix the unit would create a
	// container called just `web`, colliding with any other project's `web`
	// service and diverging from the running-stack name. An explicit
	// `container_name:` still wins.
	container.add(
		"ContainerName",
		service
			.container_name
			.clone()
			.unwrap_or_else(|| format!("{project}-{name}")),
	);
	// A service with a buildable `build:` references its `.build` unit, so Quadlet
	// builds the image before running; otherwise the explicit `image:` is used.
	if super::build::emits_build_unit(service) {
		container.add("Image", super::build::build_unit_filename(project, name));
	} else if let Some(image) = &service.image {
		container.add("Image", image.clone());
	}
	if let Some(hostname) = &service.hostname {
		container.add("HostName", hostname.clone());
	}
	if let Some(user) = &service.user {
		// Quadlet `User=` takes a UID/username only; a `uid:gid` compose value
		// must be split so the GID lands in the dedicated `Group=` key (Quadlet
		// recombines them into `--user uid:gid`).
		match user.split_once(':') {
			Some((uid, gid)) => {
				container.add("User", uid.to_string());
				container.add("Group", gid.to_string());
			}
			None => container.add("User", user.clone()),
		}
	}
	if let Some(wd) = &service.working_dir {
		container.add("WorkingDir", wd.clone());
	}
	if service.read_only == Some(true) {
		container.add("ReadOnly", "true".to_string());
	}
	if service.privileged == Some(true) {
		// No dedicated [Container] key exists for privileged mode; pass it through
		// as a raw podman flag, like the other escape-hatch fields.
		container.add("PodmanArgs", "--privileged".to_string());
		// The live `up` engine emits a per-call warning for every active
		// host-binding mode. Quadlet cannot call the engine (it has no Podman
		// client), so it surfaces the same warning here, at generate time, so
		// the operator sees the warning whichever path they use. The mode is
		// still emitted below; the warning is the point. `--no-warn` opts
		// the operator out of this one too (set via the `NoWarnGuard` the CLI
		// driver wraps around `write_quadlet`).
		if !is_no_warn_set() {
			tracing::warn!(
				"service \"{name}\": privileged: true grants every Linux capability and exposes \
				 every host device; under rootless Podman the effect is reduced but the container \
				 still bypasses the default capability set"
			);
		}
	}
	if service.init == Some(true) {
		container.add("RunInit", "true".to_string());
	}

	// Ports are validated (range, format) before generation, so parsing succeeds
	// here. A malformed/out-of-range mapping is rejected at the command boundary
	// rather than re-emitted verbatim as an invalid `PublishPort=` — emitting the
	// raw string would produce a unit Quadlet/Podman would reject anyway.
	if let Ok(ports) = parse_ports(&service.ports) {
		for p in ports {
			container.add("PublishPort", render_publish_port(&p));
		}
	}

	for (key, val) in sorted_pairs(service.environment.to_map()) {
		match val {
			Some(v) => container.add("Environment", format!("{key}={v}")),
			None => container.add("Environment", key),
		}
	}

	for vol in &service.volumes {
		// A long-form `type: tmpfs` mount maps to `Tmpfs=`, not `Volume=`
		// (which would persist it as a volume rather than an in-memory fs).
		if let Some(t) = render_tmpfs_mount(vol) {
			container.add("Tmpfs", t);
		} else {
			container.add("Volume", render_volume(vol, project, declared_volumes));
		}
	}
	for net in service.networks.names() {
		// A declared (non-external) network is backed by a generated `.network`
		// unit; an external network is referenced by its existing name directly,
		// since no unit is emitted for it.
		if declared_networks.contains(&net.as_str()) {
			container.add("Network", format!("{}.network", unit_stem(project, &net)));
		} else {
			container.add("Network", net.clone());
		}
	}
	for (key, val) in sorted_label_pairs(service.labels.to_map()) {
		container.add("Label", format!("{key}={val}"));
	}
	// Ownership labels, mirroring the live engine: tag every generated container
	// with its project and service so it is traceable/removable by label the same
	// way a running one is.
	container.add("Label", format!("podup.project={project}"));
	container.add("Label", format!("podup.service={name}"));
	for cap in &service.cap_add {
		container.add("AddCapability", cap.clone());
	}
	for cap in &service.cap_drop {
		container.add("DropCapability", cap.clone());
	}
	if let Some(entrypoint) = &service.entrypoint {
		container.add("Entrypoint", render_command(entrypoint));
	}
	if let Some(command) = &service.command {
		container.add("Exec", render_command(command));
	}

	for ann in sorted_label_pairs(service.annotations.to_map()) {
		container.add("Annotation", format!("{}={}", ann.0, ann.1));
	}
	// `EnvironmentFile=` is resolved by podman-systemd.unit(5) against the unit
	// file's own directory, not the compose file's. Units are installed to
	// `~/.config/containers/systemd`, so `env_file: .env` would render to a unit
	// looking for `~/.config/containers/systemd/.env` — and `--env-file` on a
	// missing path is fatal, so the container never starts. Resolve against the
	// compose base directory, the same way the build context is.
	for entry in service.env_file.to_entries() {
		container.add("EnvironmentFile", abs_against(base_dir, entry.path()));
	}
	for t in service.tmpfs.to_list() {
		container.add("Tmpfs", t);
	}
	for (key, val) in sorted_label_pairs(service.sysctls.to_map()) {
		container.add("Sysctl", format!("{key}={val}"));
	}
	for (name, limit) in &service.ulimits {
		let soft = limit.soft();
		let hard = limit.hard();
		let value = if soft == hard {
			format!("{name}={soft}")
		} else {
			format!("{name}={soft}:{hard}")
		};
		container.add("Ulimit", value);
	}
	for dev in &service.devices {
		container.add("AddDevice", dev.clone());
	}
	for host in &service.extra_hosts {
		container.add("AddHost", host.clone());
	}
	for d in service.dns.to_list() {
		container.add("DNS", d);
	}
	for d in service.dns_search.to_list() {
		container.add("DNSSearch", d);
	}
	for d in service.dns_opt.to_list() {
		container.add("DNSOption", d);
	}
	if let Some(shm) = &service.shm_size {
		container.add("ShmSize", shm.clone());
	}
	if let Some(mem) = &service.mem_limit {
		// `Memory=` is not a recognised [Container] Quadlet key (Quadlet would drop
		// the whole unit at daemon-reload), so route the limit through PodmanArgs=
		// as `--memory`, like the CPU limits. The value is validated as a size
		// before generation, so it is a well-formed limit here.
		container.add("PodmanArgs", format!("--memory={mem}"));
	}
	// CPU limits have no native [Container] Quadlet key (unlike Memory=/
	// PidsLimit=), so they go through PodmanArgs=.
	// `cpus` falls back to the modern `deploy.resources.limits.cpus`.
	let deploy_cpus = service
		.deploy
		.as_ref()
		.and_then(|d| d.resources.as_ref())
		.and_then(|r| r.limits.as_ref())
		.and_then(|l| l.cpus.as_deref());
	if let Some(c) = service.cpus.as_deref().or(deploy_cpus) {
		container.add("PodmanArgs", format!("--cpus={c}"));
	}
	if let Some(cs) = &service.cpuset {
		container.add("PodmanArgs", format!("--cpuset-cpus={cs}"));
	}
	if let Some(sh) = service.cpu_shares {
		container.add("PodmanArgs", format!("--cpu-shares={sh}"));
	}
	if let Some(q) = service.cpu_quota {
		container.add("PodmanArgs", format!("--cpu-quota={q}"));
	}
	if let Some(p) = service.cpu_period {
		container.add("PodmanArgs", format!("--cpu-period={p}"));
	}
	// `deploy.resources.limits.pids` is the modern equivalent of `pids_limit`.
	let deploy_pids = service
		.deploy
		.as_ref()
		.and_then(|d| d.resources.as_ref())
		.and_then(|r| r.limits.as_ref())
		.and_then(|l| l.pids);
	if let Some(pids) = service.pids_limit {
		container.add("PidsLimit", pids.to_string());
	} else if let Some(pids) = deploy_pids {
		container.add("PidsLimit", pids.to_string());
	}
	if let Some(userns) = &service.userns_mode {
		container.add("UserNS", userns.clone());
	}
	if let Some(signal) = &service.stop_signal {
		container.add("StopSignal", signal.clone());
	}
	if let Some(grace) = &service.stop_grace_period {
		if let Some(secs) = parse_duration_secs(grace) {
			container.add("StopTimeout", secs.to_string());
		}
	}
	// `network_mode: host`/`none` map to `Network=host`/`Network=none`.
	// `service:X` reuses a *sibling service's* netns, which Quadlet expresses as
	// `Network={X}.container` (the `.container` unit dependency). `container:X`
	// reuses an *existing* container's netns by id/name and maps to podman's
	// `Network=container:X` join form — not a `.container` unit, which would name
	// a non-existent dependency and fail to start. Other modes (bridge:, custom,
	// …) have no key and are reported by collect_warnings.
	match service.network_mode.as_deref() {
		Some("host") => {
			container.add("Network", "host".to_string());
			// Quadlet emits the unit file with `Network=host` and walks away;
			// there is no engine call to warn on it. The warning is the same
			// one the live `up` path emits, so the operator sees an identical
			// message whichever path they use. `--no-warn` opts the operator
			// out of this one too.
			if !is_no_warn_set() {
				tracing::warn!(
					"service \"{name}\": network_mode: host shares the host's network namespace; \
					 the container sees host network interfaces and any port it binds is a host port"
				);
			}
		}
		Some("none") => container.add("Network", "none".to_string()),
		Some(m) => {
			if let Some(target) = m.strip_prefix("service:") {
				container.add(
					"Network",
					format!("{}.container", unit_stem(project, target)),
				);
			} else if let Some(target) = m.strip_prefix("container:") {
				container.add("Network", format!("container:{target}"));
				// Sharing another container's netns collides with the same
				// isolation argument as `host`. Podman does not warn on it at
				// generate time, so the surface lives here. `--no-warn` opts
				// the operator out of this one too.
				if !is_no_warn_set() {
					tracing::warn!(
						"service \"{name}\": network_mode: container:{target} shares another \
						 container's network namespace; both containers see the same network \
						 interfaces and ports"
					);
				}
			}
		}
		None => {}
	}
	for group in &service.group_add {
		container.add("GroupAdd", group.clone());
	}
	for port in &service.expose {
		container.add("ExposeHostPort", port.clone());
	}
	// `IP=`/`IP6=` are single-valued per container, so the first static address
	// declared across the service's networks wins (Quadlet has no per-network IP
	// scoping); a second one is reported by collect_warnings.
	let mut static_ip: Option<&str> = None;
	let mut static_ip6: Option<&str> = None;
	// Emit each alias at most once: a repeated alias (within a network or across
	// networks) would produce duplicate `NetworkAlias=` lines, which podman may
	// reject at container create.
	let mut seen_aliases = std::collections::HashSet::new();
	for net in service.networks.names() {
		if let Some(cfg) = service.networks.config_for(&net) {
			if let Some(aliases) = &cfg.aliases {
				for alias in aliases {
					if seen_aliases.insert(alias.clone()) {
						container.add("NetworkAlias", alias.clone());
					}
				}
			}
			if static_ip.is_none() {
				static_ip = cfg.ipv4_address.as_deref();
			}
			if static_ip6.is_none() {
				static_ip6 = cfg.ipv6_address.as_deref();
			}
		}
	}
	if let Some(ip) = static_ip {
		container.add("IP", ip.to_string());
	}
	if let Some(ip6) = static_ip6 {
		container.add("IP6", ip6.to_string());
	}
	for opt in &service.security_opt {
		map_security_opt(opt, &mut container, name, warnings);
	}
	// `build_log_config` substitutes the rotation default when `service.logging`
	// is None, so an absent `logging:` block in compose still produces a
	// `LogDriver=` / `LogOpt=` set on the generated unit (#1354). The render
	// path is the same as the live engine's, so `up` and `generate quadlet`
	// produce equivalent rotation policy.
	if let Some(logging) = build_log_config(service.logging.as_ref()) {
		if let Some(driver) = &logging.driver {
			container.add("LogDriver", driver.clone());
		}
		for (key, val) in sorted_label_pairs(logging.options) {
			container.add("LogOpt", format!("{key}={val}"));
		}
	}
	if let Some(pull) = &service.pull_policy {
		container.add("Pull", pull.clone());
	}
	// `deploy.resources.limits.memory` is the modern equivalent of `mem_limit`.
	if service.mem_limit.is_none() {
		if let Some(mem) = service
			.deploy
			.as_ref()
			.and_then(|d| d.resources.as_ref())
			.and_then(|r| r.limits.as_ref())
			.and_then(|l| l.memory.as_ref())
		{
			container.add("PodmanArgs", format!("--memory={mem}"));
		}
	}
	for secret in &service.secrets {
		// An inline (`content:`/`environment:`) secret is created by `up` under the
		// project-scoped name `{project}_secret_{name}`; reference that here so the
		// generated unit points at the secret `up` would create, not an unscoped
		// (possibly colliding or non-existent) host secret. Quadlet does not create
		// the secret itself, so warn the operator to provision it first.
		let source = secret.source();
		if is_inline_secret(secrets.get(source)) {
			warnings.push(format!(
				"{name}: inline secret {source:?} is referenced but Quadlet does not \
				 create it; provision the project-scoped secret \"{project}_secret_{source}\" \
				 first (e.g. via `podup up`)"
			));
		}
		container.add("Secret", render_secret(secret, project, secrets));
	}
	render_healthcheck(name, service, &mut container, warnings);

	let mut svc = Section::new("Service");
	if let Some(restart) = &service.restart {
		svc.add("Restart", render_restart(restart));
		if let RestartPolicy::OnFailure {
			max_attempts: Some(n),
		} = restart
		{
			svc.add("StartLimitBurst", n.to_string());
		}
	} else if let Some(rp) = service
		.deploy
		.as_ref()
		.and_then(|d| d.restart_policy.as_ref())
	{
		// `deploy.restart_policy` is the modern equivalent of the service-level
		// `restart:` string; its `condition` maps onto the systemd `Restart=`
		// values and `max_attempts`/`window` onto the start-limit window.
		let restart = match rp.condition.as_deref() {
			Some("none") => "no",
			Some("on-failure") => "on-failure",
			// "any" (the compose default) and any unknown value restart always.
			_ => "always",
		};
		svc.add("Restart", restart.to_string());
		if let Some(n) = rp.max_attempts {
			svc.add("StartLimitBurst", n.to_string());
			if let Some(secs) = rp.window.as_deref().and_then(parse_duration_secs) {
				svc.add("StartLimitIntervalSec", secs.to_string());
			}
		}
	}

	collect_warnings(name, service, warnings);

	// The unforgeable ownership marker comes first, as its own comment line;
	// see `owner_marker` for why it must stay separate from the `Label=` line.
	let mut contents = owner_marker(project);
	contents.push_str(&unit.render());
	contents.push('\n');
	contents.push_str(&container.render());
	if !svc.is_empty() {
		contents.push('\n');
		contents.push_str(&svc.render());
	}
	contents.push_str("\n[Install]\nWantedBy=default.target\n");

	QuadletUnit {
		filename: format!("{}.container", unit_stem(project, name)),
		contents,
	}
}
