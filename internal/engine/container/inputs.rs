//! Per-container input computation, split out of [`super::mod`] so the
//! orchestrator stays under the 400-line ceiling while the helpers stay
//! close to the libpod types they build.

use std::collections::HashMap;

use crate::compose::types::{ComposeFile, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::{LinuxDevice, LinuxDeviceCgroup, PortMapping};
use crate::ports;

use super::super::Engine;
use super::fields::{build_label_file_labels, encode_path_for_label, resolve_container_labels};
use super::resolve::{config_hash, resolve_volume_name};
use super::security::parse_device_cgroup_rule;
use crate::engine::container::fields::parse_device;
use crate::engine::container::security::cdi_device;
use crate::engine::container_config::cdi_devices;

impl Engine {
	/// Resolve a service's named-volume reference to the actual volume name
	/// that `create_volumes` produced: a custom `name:`, the raw name for an
	/// external volume, or the `{project}_{name}` form. An empty reference is an
	/// anonymous volume and is left unchanged; a non-empty reference that is not
	/// declared under the top-level `volumes:` map is rejected (compose-spec).
	pub(super) fn resolved_volume_name(
		&self,
		reference: &str,
		file: &ComposeFile,
	) -> Result<String> {
		resolve_volume_name(reference, &self.project, file)
	}

	/// Compute the final `labels:` map for a container, merging the
	/// compose-file labels, the `x-podman-autoupdate` extension, the
	/// `podup.*` ownership labels, and the `podup.config-files` path list.
	pub(super) fn compute_container_labels(
		&self,
		service: &Service,
		file: &ComposeFile,
		service_name: &str,
	) -> Result<HashMap<String, String>> {
		let label_file_labels = build_label_file_labels(service, &self.base_dir)?;
		// Per the Compose Specification, deploy.labels are set on the service
		// only and must NOT be applied to containers, so they are not merged here.
		let mut labels = resolve_container_labels(service, label_file_labels);
		// The `x-podman-autoupdate` extension. Rejected at create time when the
		// value is not one of Podman's two policies, so a typo cannot silently
		// leave a container invisible to `podman auto-update`. Mirrors how
		// `x-podman-on-failure` is rejected above (`health_check_on_failure_action`).
		match service.podman_autoupdate() {
			Ok(Some(policy)) => {
				labels.insert(
					"io.containers.autoupdate".to_string(),
					policy.as_str().to_string(),
				);
			}
			Ok(None) => {}
			Err(e) => {
				return Err(ComposeError::Unsupported(format!("{service_name}: {e}")));
			}
		}
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
		Ok(labels)
	}

	/// Compute the libpod `portmappings` and `expose` maps for one service.
	/// In pod mode the pod publishes every service's ports, so the per-container
	/// `portmappings` is empty; `expose` is built either way so the
	/// image-side `EXPOSE` shape stays visible to the engine.
	pub(super) fn compute_portmappings_and_expose(
		&self,
		service: &Service,
		in_pod: bool,
	) -> Result<(Vec<PortMapping>, HashMap<u16, String>)> {
		let parsed_ports = ports::parse_ports(&service.ports)?;
		let portmappings = if in_pod {
			Vec::new()
		} else {
			ports::to_libpod(&parsed_ports)
		};
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
		Ok((portmappings, expose))
	}

	/// Compute the device list and the device cgroup rules for one service:
	/// the per-device OCI nodes (with the cgroup rule split off), the
	/// CDI device names, and the structured `device_cgroup_rules:` entries
	/// (malformed entries are warned and dropped).
	pub(super) fn compute_devices_and_rules(
		&self,
		service: &Service,
	) -> (Vec<LinuxDevice>, Vec<LinuxDeviceCgroup>) {
		let mut devices: Vec<LinuxDevice> = Vec::with_capacity(service.devices.len());
		// A device's `:permissions` segment cannot live on the OCI node, so it
		// rides alongside as a cgroup access rule (mirroring the quadlet backend's
		// verbatim `AddDevice`). These are merged with the explicit
		// `device_cgroup_rules` below.
		let mut device_cgroup_rule: Vec<LinuxDeviceCgroup> = Vec::new();
		for raw in &service.devices {
			let parsed = parse_device(raw);
			if let Some(rule) = parsed.cgroup_rule {
				device_cgroup_rule.push(rule);
			}
			devices.push(parsed.device);
		}
		// CDI device names ride in the same array; Podman pulls them out by path.
		devices.extend(cdi_devices(service).into_iter().map(cdi_device));
		device_cgroup_rule.extend(service.device_cgroup_rules.iter().filter_map(|r| {
			parse_device_cgroup_rule(r).or_else(|| {
				tracing::warn!("device_cgroup_rules entry '{r}' is malformed and is ignored");
				None
			})
		}));
		(devices, device_cgroup_rule)
	}
}
