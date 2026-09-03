//! Container creation and start: assembles a libpod `SpecGenerator` from a
//! [`Service`] and starts the container.

use std::collections::HashMap;

use crate::compose::types::{ComposeFile, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::{LinuxResources, Namespace};
use crate::libpod::urlencoded;
use crate::libpod::validate::pre_validate_spec;
use crate::libpod::API_PREFIX;
use crate::size;

mod fields;
mod host_mode;
mod inputs;
mod resolve;
mod rootless;
mod security;
mod spec;
use resolve::{build_env, resolve_links, resolve_stop_signal, resolve_volumes_from};
pub(crate) use resolve::{config_hash, resolve_bind_source};
pub(crate) use spec::{build_spec_generator, NamespaceInputs, SecurityInputs, SpecInputs};

pub(crate) use host_mode::check_host_mode;

use super::container_config::{
	build_healthcheck, build_log_config, build_resource_limits, build_restart_policy, build_ulimits,
};
use super::network::resolve_network_mode;
use super::volume_mounts::build_mounts_all;
use super::Engine;
use fields::{build_blkio_config, warn_swarm_only_deploy};
use security::parse_security_opts;

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
		// In pod mode the pod publishes every service's ports: a container
		// inside a pod cannot publish ports of its own, so the per-container
		// list is empty. The pod already aggregated the union via
		// `Engine::ensure_pod`; sending ports here would conflict.
		let in_pod = file
			.podman_pod()
			.map_err(crate::error::ComposeError::Unsupported)?;
		let (portmappings, expose) = self.compute_portmappings_and_expose(service, in_pod)?;

		// --- Restart policy ---
		let (restart_policy, restart_tries) = build_restart_policy(service);

		// --- Logging ---
		let log_configuration = build_log_config(service_name, service.logging.as_ref())?;

		// --- Networks ---
		// In pod mode the container joins the pod's shared namespace, so its
		// own `networks:` map and `netns` are empty. Pod mode rejects any
		// service carrying `network_mode:` up front (see
		// `validate_pod_or_refuse`), so the only branch here is "no
		// per-container networks".
		let (netns, networks) = if in_pod {
			(None, HashMap::new())
		} else {
			resolve_network_mode(service_name, service, file, &self.project)
		};

		// --- Labels ---
		let labels = self.compute_container_labels(service, file, service_name)?;

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
		let (devices, device_cgroup_rule) = self.compute_devices_and_rules(service);

		// --- Security options (decomposed onto SpecGenerator fields) ---
		let security = parse_security_opts(service);

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

		// --- Namespace modes, platform, links, volumes_from ---
		let pidns = service.pid.as_deref().map(Namespace::parse);
		let ipcns = service.ipc.as_deref().map(Namespace::parse);
		let utsns = service.uts.as_deref().map(Namespace::parse);
		let cgroupns = service.cgroup.as_deref().map(Namespace::parse);
		let userns = service.userns_mode.as_deref().map(Namespace::parse);
		let (image_os, image_arch) = service
			.platform
			.as_deref()
			.and_then(|p| p.split_once('/'))
			.map(|(os, arch)| (Some(os.to_string()), Some(arch.to_string())))
			.unwrap_or((None, None));
		let links = resolve_links(service, file, &self.project);
		let volumes_from = resolve_volumes_from(service, file, &self.project);
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

		for warning in rootless::rootless_caveat_warnings(service_name, service) {
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

		let spec = build_spec_generator(
			&self.project,
			service,
			service.healthcheck.as_ref().map(build_healthcheck),
			log_configuration,
			match service
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
			SpecInputs {
				in_pod,
				container_name: container_name.to_string(),
				image: image.to_string(),
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
				security: SecurityInputs {
					selinux_opts: security.selinux_opts,
					apparmor_profile: security.apparmor_profile,
					seccomp_profile_path: security.seccomp_profile_path,
					no_new_privileges: security.no_new_privileges,
					mask: security.mask,
					unmask: security.unmask,
				},
				namespaces: NamespaceInputs {
					userns,
					pidns,
					ipcns,
					utsns,
					cgroupns,
				},
				restart: (restart_policy, restart_tries),
				stop_signal_timeout: (stop_signal, stop_timeout),
				command: service.command.as_ref().map(|c| c.to_exec()),
				entrypoint: service.entrypoint.as_ref().map(|c| c.to_exec()),
				env,
				links,
				image_platform: (image_os, image_arch),
				storage_opts: service.storage_opt.clone(),
			},
		)?;

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
}
