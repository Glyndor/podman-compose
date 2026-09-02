//! Container creation and start: assembles a libpod `SpecGenerator` from a
//! [`Service`] and starts the container.

use std::collections::HashMap;

use crate::compose::types::{ComposeFile, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::{LinuxResources, Namespace, SpecGenerator};
use crate::libpod::urlencoded;
use crate::libpod::validate::pre_validate_spec;
use crate::libpod::API_PREFIX;
use crate::{ports, size};

mod fields;
mod host_mode;
mod resolve;
mod security;
use resolve::{
	build_env, resolve_links, resolve_stop_signal, resolve_volume_name, resolve_volumes_from,
};
pub(crate) use resolve::{config_hash, resolve_bind_source};

pub(crate) use host_mode::check_host_mode;

use super::container_config::{
	build_healthcheck, build_log_config, build_resource_limits, build_restart_policy,
	build_ulimits, cdi_devices,
};
use super::network::resolve_network_mode;
use super::volume_mounts::build_mounts_all;
use super::Engine;
use fields::{
	build_blkio_config, build_label_file_labels, encode_path_for_label, parse_device,
	resolve_container_labels, warn_swarm_only_deploy,
};
use security::{cdi_device, parse_device_cgroup_rule, parse_security_opts};

impl Engine {
	pub(super) async fn create_and_start(
		&self,
		container_name: &str,
		service_name: &str,
		service: &Service,
		file: &ComposeFile,
		start: bool,
	) -> Result<()> {
		// Reject an invalid container name client-side (podman would otherwise
		// answer with an opaque HTTP 500 from its name-regex check). Covers the
		// ad-hoc `run --name` path too, which never passes through the up-time
		// preflight in `validate_object_names`.
		super::names::ensure_valid_object_name("container", service_name, container_name)?;

		let derived_image;
		let image: &str = if let Some(img) = service.image.as_deref() {
			img
		} else if let Some(build) = service.build.as_ref() {
			// No `image:` — reference the exact tag the build step produced for this
			// build-only service (project-scoped `{project}-{service}:latest`, or
			// the first `build.tags` entry). Must stay in lockstep with
			// `primary_build_tag`, or `up --build` creates the container against a
			// name no built image carries (HTTP 404: no such image).
			derived_image =
				super::build::primary_build_tag(&self.project, service_name, None, build.tags());
			&derived_image
		} else {
			return Err(ComposeError::NoImageOrBuild(service_name.into()));
		};

		warn_swarm_only_deploy(service_name, service);

		// --- Environment ---
		// A bare `KEY` (no `=`) is a passthrough: its value comes from podup's
		// own environment, matching docker-compose. Drop it only when unset.
		let env: HashMap<String, String> = build_env(service, &self.base_dir)?
			.into_iter()
			.filter_map(|s| match s.find('=') {
				Some(idx) => Some((s[..idx].to_string(), s[idx + 1..].to_string())),
				None => std::env::var(&s).ok().map(|v| (s, v)),
			})
			.collect();

		// Every secret/config source — inline, `file:` and `external: true` — is
		// injected as a Podman-native secret, never a bind mount. The ones podup
		// owns are created up front by `create_project_secrets`; here we only build
		// the references and preflight external ones for existence.
		let native_secrets = self.build_native_secrets(service, file).await?;
		let (mut mounts, mut named_volumes) = build_mounts_all(service, &self.base_dir);
		// Resolve relative bind sources against the project base directory (and
		// expand a leading `~`) so they don't depend on Podman's working
		// directory; absolute paths (incl. staged secrets/configs) are untouched.
		for m in &mut mounts {
			if m.mount_type == "bind" {
				if let Some(src) = m.source.take() {
					m.source = Some(resolve_bind_source(&src, &self.base_dir));
				}
			}
		}
		// Map each named-volume reference to the actual volume name created by
		// create_volumes (project-prefixed, custom `name:`, or external).
		for nv in &mut named_volumes {
			nv.name = self.resolved_volume_name(&nv.name, file)?;
		}

		// --- Port mappings ---
		let parsed_ports = ports::parse_ports(&service.ports)?;
		let portmappings = ports::to_libpod(&parsed_ports);

		// expose map: port_num → protocol
		let mut expose: HashMap<u16, String> = parsed_ports
			.iter()
			.map(|p| (p.container_port, p.protocol.clone()))
			.collect();
		for raw in &service.expose {
			let (port_str, proto) = if let Some(idx) = raw.rfind('/') {
				(&raw[..idx], raw[idx + 1..].to_string())
			} else {
				(raw.as_str(), "tcp".to_string())
			};
			if let Ok(p) = port_str.parse::<u16>() {
				expose.entry(p).or_insert(proto);
			}
		}

		// --- Restart policy ---
		let (restart_policy, restart_tries) = build_restart_policy(service);

		// --- Logging ---
		let log_configuration = build_log_config(service_name, service.logging.as_ref())?;

		// --- Networks ---
		let (netns, networks) = resolve_network_mode(service_name, service, file, &self.project);

		// --- Labels ---
		let label_file_labels = build_label_file_labels(service, &self.base_dir)?;
		// Per the Compose Specification, deploy.labels are set on the service
		// only and must NOT be applied to containers, so they are not merged here.
		let mut labels = resolve_container_labels(service, label_file_labels);
		labels.insert("podup.project".to_string(), self.project.clone());
		labels.insert("podup.service".to_string(), service_name.to_string());
		labels.insert("podup.config-hash".to_string(), config_hash(service, file)?);
		// Where this project's compose file lives. `ls` discovers projects purely
		// by label and keeps no other record, so without this its `ConfigFiles`
		// column can only ever be blank. Omitted rather than written empty when the
		// caller did not supply the paths, so a reader can tell "not recorded" from
		// "recorded as nothing".
		if !self.compose_files.is_empty() {
			let joined = self
				.compose_files
				.iter()
				// URL-encode any `,` so a path containing one cannot visually merge
				// with the next entry when the joined label is split back on `,`.
				.map(|p| encode_path_for_label(&p.display().to_string()))
				.collect::<Vec<_>>()
				.join(",");
			labels.insert("podup.config-files".to_string(), joined);
		}

		// annotations
		let annotations: HashMap<String, String> = service.annotations.to_map();

		// --- Sysctls ---
		let sysctl: HashMap<String, String> = service.sysctls.to_map();

		// --- Resource limits ---
		let mut resource_limits = build_resource_limits(service);
		if let Some(blkio) = build_blkio_config(service) {
			resource_limits
				.get_or_insert_with(LinuxResources::default)
				.block_io = Some(blkio);
		}

		// --- Ulimits ---
		let ulimits = build_ulimits(service);

		// --- Devices ---
		let mut devices: Vec<_> = Vec::with_capacity(service.devices.len());
		// A device's `:permissions` segment cannot live on the OCI node, so it
		// rides alongside as a cgroup access rule (mirroring the quadlet backend's
		// verbatim `AddDevice`). These are merged with the explicit
		// `device_cgroup_rules` below.
		let mut device_cgroup_rule: Vec<_> = Vec::new();
		for raw in &service.devices {
			let parsed = parse_device(raw);
			if let Some(rule) = parsed.cgroup_rule {
				device_cgroup_rule.push(rule);
			}
			devices.push(parsed.device);
		}
		// CDI device names ride in the same array; Podman pulls them out by path.
		devices.extend(cdi_devices(service).into_iter().map(cdi_device));

		// --- Security options (decomposed onto SpecGenerator fields) ---
		let security = parse_security_opts(service);

		// --- Device cgroup rules (parsed to structured form; skip malformed) ---
		device_cgroup_rule.extend(service.device_cgroup_rules.iter().filter_map(|r| {
			parse_device_cgroup_rule(r).or_else(|| {
				tracing::warn!("device_cgroup_rules entry '{r}' is malformed and is ignored");
				None
			})
		}));

		// Pre-validate the `SpecGenerator` fields libpod validates on its own
		// (namespace modes, `device_cgroup_rule` access strings), so a rejected
		// value surfaces as a `PodmanError::Field` carrying the compose-side
		// field name and offending value instead of libpod's raw validator
		// text. Podup's `SpecGenerator` is built below; doing this before
		// assembling it keeps the pre-validator close to the service fields
		// it inspects (#1357).
		let device_cgroup_access: Vec<String> = device_cgroup_rule
			.iter()
			.filter_map(|r| r.access.clone())
			.collect();
		pre_validate_spec(service_name, service, &device_cgroup_access)?;

		// --- Namespace modes ---
		let pidns = service.pid.as_deref().map(Namespace::parse);
		let ipcns = service.ipc.as_deref().map(Namespace::parse);
		let utsns = service.uts.as_deref().map(Namespace::parse);
		let cgroupns = service.cgroup.as_deref().map(Namespace::parse);
		let userns = service.userns_mode.as_deref().map(Namespace::parse);

		// --- Platform → os / arch ---
		let (image_os, image_arch) = service
			.platform
			.as_deref()
			.and_then(|p| p.split_once('/'))
			.map(|(os, arch)| (Some(os.to_string()), Some(arch.to_string())))
			.unwrap_or((None, None));

		// --- Links ---
		let links = resolve_links(service, file, &self.project);

		// --- volumes_from ---
		let volumes_from = resolve_volumes_from(service, file, &self.project);

		// --- SHM size ---
		let shm_size = service.shm_size.as_deref().and_then(size::parse_memory);

		// --- Stop timeout ---
		let stop_timeout = service
			.stop_grace_period
			.as_deref()
			.and_then(size::parse_duration_secs);

		if service.mac_address.is_some() {
			tracing::warn!(
				"service \"{service_name}\": top-level mac_address is deprecated; \
				move it to networks.<network>.mac_address"
			);
		}

		for warning in rootless_caveat_warnings(service_name, service) {
			tracing::warn!("{warning}");
		}

		// Surface every active host-binding / privilege-escalation mode the
		// compose file declared. The warning is emitted *before* the spec is
		// POSTed so a host-mode the operator did not intend never reaches the
		// daemon — the log line is the only signal they get, and it has to
		// arrive before the API call succeeds. `--no-warn` is the escape
		// hatch for operators who wrote the compose file deliberately.
		if !self.no_warn {
			for w in check_host_mode(service_name, service) {
				tracing::warn!("{}", w.message);
			}
		}

		let stop_signal = service
			.stop_signal
			.as_deref()
			.map(resolve_stop_signal)
			.transpose()?;

		let spec = SpecGenerator {
			name: container_name.to_string(),
			image: image.to_string(),
			command: service.command.as_ref().map(|c| c.to_exec()),
			entrypoint: service.entrypoint.as_ref().map(|c| c.to_exec()),
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
			selinux_opts: security.selinux_opts,
			apparmor_profile: security.apparmor_profile,
			seccomp_profile_path: security.seccomp_profile_path,
			no_new_privileges: security.no_new_privileges,
			mask: security.mask,
			unmask: security.unmask,
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
			userns,
			pidns,
			ipcns,
			utsns,
			cgroupns,
			cgroup_parent: service.cgroup_parent.clone(),
			resource_limits,
			ulimits,
			shm_size,
			healthconfig: service.healthcheck.as_ref().map(build_healthcheck),
			// The `x-podman-on-failure` extension. Rejected at create time when the
			// value is not one of Podman's four actions, so a typo cannot silently
			// leave a sick container in rotation.
			health_check_on_failure_action: match service
				.healthcheck
				.as_ref()
				.map(crate::compose::types::HealthCheck::podman_on_failure)
				.transpose()
			{
				Ok(action) => action.flatten().map(|a| match a {
					crate::compose::types::HealthOnFailure::None => {
						crate::libpod::types::container::HealthCheckOnFailureAction::None
					}
					crate::compose::types::HealthOnFailure::Kill => {
						crate::libpod::types::container::HealthCheckOnFailureAction::Kill
					}
					crate::compose::types::HealthOnFailure::Restart => {
						crate::libpod::types::container::HealthCheckOnFailureAction::Restart
					}
					crate::compose::types::HealthOnFailure::Stop => {
						crate::libpod::types::container::HealthCheckOnFailureAction::Stop
					}
				}),
				Err(e) => {
					return Err(crate::error::ComposeError::Unsupported(format!(
						"{service_name}: {e}"
					)))
				}
			},
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
			storage_opts: service.storage_opt.clone(),
			..Default::default()
		};

		// Remove any existing container (idempotent restart). `up
		// -V/--renew-anon-volumes` also drops its old anonymous volumes (v=true)
		// so they are recreated fresh instead of orphaned.
		let rm_path = format!(
			"{API_PREFIX}/containers/{}?force=true&v={}",
			urlencoded(container_name),
			self.renew_anon_volumes,
		);
		if let Err(e) = self.client.delete_ok(&rm_path).await {
			tracing::debug!("pre-create delete {container_name}: {e}");
		}

		self.client
			.post_json::<_, serde_json::Value>(&format!("{API_PREFIX}/containers/create"), &spec)
			.await
			.map_err(ComposeError::Podman)?;

		// `create` (docker compose create) creates the container but leaves it
		// stopped; `up`/`run`/`watch` start it.
		if start {
			let start_path = format!(
				"{API_PREFIX}/containers/{}/start",
				urlencoded(container_name)
			);
			self.client
				.post_empty_ok(&start_path)
				.await
				.map_err(ComposeError::Podman)?;
		}

		Ok(())
	}

	/// Resolve a service's named-volume reference to the actual volume name
	/// that `create_volumes` produced: a custom `name:`, the raw name for an
	/// external volume, or the `{project}_{name}` form. An empty reference is an
	/// anonymous volume and is left unchanged; a non-empty reference that is not
	/// declared under the top-level `volumes:` map is rejected (compose-spec).
	fn resolved_volume_name(&self, reference: &str, file: &ComposeFile) -> Result<String> {
		resolve_volume_name(reference, &self.project, file)
	}
}

/// Compose fields Podman accepts but cannot honor (or that fail) under rootless
/// Podman on cgroups v2. Returns advisory messages; pure so it can be
/// unit-tested. The wording mirrors podman-run(1) so operators are not misled
/// into assuming a no-op limit took effect.
fn rootless_caveat_warnings(name: &str, service: &Service) -> Vec<String> {
	let mut out = Vec::new();
	if service.privileged == Some(true) {
		out.push(format!(
			"service \"{name}\": privileged has reduced effect under rootless Podman — a \
			container cannot gain more privileges than the user that launched it"
		));
	}
	if service.oom_kill_disable.is_some() {
		out.push(format!(
			"service \"{name}\": oom_kill_disable is not supported on cgroups v2 systems and \
			is ignored"
		));
	}
	if service.mem_swappiness.is_some() {
		out.push(format!(
			"service \"{name}\": mem_swappiness is only supported on cgroups v1 rootful systems \
			and is ignored otherwise"
		));
	}
	if service.cpu_rt_runtime.is_some() || service.cpu_rt_period.is_some() {
		out.push(format!(
			"service \"{name}\": cpu_rt_runtime/cpu_rt_period are only supported on cgroups v1 \
			rootful systems; the container may fail to start rootless"
		));
	}
	if !service.links.is_empty() {
		out.push(format!(
			"service \"{name}\": links has no effect under rootless Podman networking — put the \
			services on a shared network and reach them by service name instead"
		));
	}
	if !service.external_links.is_empty() {
		out.push(format!(
			"service \"{name}\": external_links has no effect under rootless Podman networking — \
			attach the target container to a shared network and reach it by service name instead"
		));
	}
	out
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
