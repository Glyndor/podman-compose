//! Build the `.pod` Quadlet unit for a project.
//!
//! Emitted alongside the `.container` units when the compose file declares
//! `x-podman-pod: true`. The unit carries every port the pod publishes
//! (the union of every service's `ports:`), every declared network
//! (`Network=`) and every `hostadd` entry (`AddHost=`); each `.container`
//! unit references this pod by `Pod=<stem>.pod` and drops its own
//! `PublishPort=` and `Network=` lines. The `.container` side of that
//! contract is in `super::container::container_unit`.

use crate::compose::types::ComposeFile;
use crate::ports;

use super::{owner_marker, render_publish_port, safe_unit_stem, unit_stem, QuadletUnit, Section};

/// Build the `.pod` unit for one project. The contents are a single
/// `[Pod]` section with `PodName=`, one `Network=` per declared network,
/// one `PublishPort=` per port (the union of every service's `ports:`),
/// and one `AddHost=` per service. Each `.container` unit references this
/// pod by `Pod=<stem>.pod`; see [`super::container::container_unit`].
///
/// Returns `None` when the project has no pod-mode extension, so callers
/// can splice the unit into the output list without a conditional.
pub(crate) fn pod_unit(project: &str, file: &ComposeFile) -> Option<QuadletUnit> {
	if !file.podman_pod().unwrap_or(false) {
		return None;
	}

	let mut pod = Section::new("Pod");
	pod.add(
		"PodName",
		// The pod is named after the project so `podman pod` and the
		// generated `Pod=<stem>.pod` references line up with what the
		// live engine creates.
		project.to_string(),
	);

	// Every non-external network the file declares, plus the external ones by
	// their own name. Mirrors the live engine's `pod_networks` builder.
	for (key, config) in &file.networks {
		let external = config.as_ref().and_then(|c| c.external).unwrap_or(false);
		if external {
			let external_name = config
				.as_ref()
				.and_then(|c| c.name.clone())
				.unwrap_or_else(|| key.clone());
			pod.add("Network", external_name);
			continue;
		}
		// A declared network is backed by a generated `.network` unit, the same
		// reference a `.container` unit makes outside pod mode.
		pod.add("Network", format!("{}.network", unit_stem(project, key)));
	}

	// Union of every service's `ports:`. The live engine hands the same
	// union to `PodSpecGenerator.portmappings`, so what we write here must
	// match what `up` would create. Sorted through the same `parsed_ports`
	// shape the engine uses for its own hash so iteration order is stable
	// across runs (an unsorted loop would flip on HashMap iteration).
	let mut ports: Vec<ports::ParsedPort> = Vec::new();
	for service in file.services.values() {
		if let Ok(parsed) = ports::parse_ports(&service.ports) {
			ports.extend(parsed);
		}
	}
	ports.sort_by(|a, b| {
		(&a.host_ip, a.host_port, a.container_port, &a.protocol).cmp(&(
			&b.host_ip,
			b.host_port,
			b.container_port,
			&b.protocol,
		))
	});
	for p in &ports {
		pod.add("PublishPort", render_publish_port(p));
	}

	// One `<service>:127.0.0.1` per service, so a compose `db:5432` reference
	// resolves to the shared namespace the way it resolves on a project
	// network. The live engine builds the same map.
	let mut hosts: Vec<String> = file
		.services
		.keys()
		.map(|s| format!("{s}:127.0.0.1"))
		.collect();
	hosts.sort();
	for entry in &hosts {
		pod.add("AddHost", entry.clone());
	}

	// Carry the project ownership label the live engine stamps onto pods, so
	// `down`/`down --remove-orphans` can find the unit by label.
	pod.add("Label", format!("podup.project={project}"));

	let mut contents = owner_marker(project);
	contents.push_str(&pod.render());
	Some(QuadletUnit {
		filename: format!("{}.pod", safe_unit_stem(project)),
		contents,
	})
}

#[cfg(test)]
#[path = "pod_tests.rs"]
mod tests;
