//! Detect compose fields that collapse the isolation between a container and
//! its host, and emit a per-mode warning so the operator can confirm the build
//! is intentional rather than discovering it at run time.
//!
//! The same modes are warned on the live `up` path (here), on the Quadlet
//! export path (where `pid`/`ipc`/`uts`/`cgroup` are dropped at the unit file
//! layer with a separate warning), and on `podup config` (where the active
//! modes are surfaced at the default log level). The three sites share one
//! detector so the messages match: an operator who sees the warning on `config`
//! sees the same line on `up`, and a host-mode they wrote deliberately can be
//! silenced the same way everywhere via `--no-warn`.

use crate::compose::types::Service;

/// One active host-binding mode detected on a service, with the operator-facing
/// message the engine emits at warning level. The detector returns a list of
/// these rather than a single `bool` so future modes (e.g. `cgroup_parent`
/// pointing at the root cgroup) can be added without changing the call sites.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeWarning {
	/// Name of the service (the compose key, not the container name) the
	/// warning is about. The engine substitutes this into the message so the
	/// rendered line is greppable in CI.
	pub service: String,
	/// Compose field name that triggered the warning (`network_mode`, `pid`,
	/// `privileged`, …). Stable for grep / log filtering.
	pub field: &'static str,
	/// The value the compose file carried, when meaningful
	/// (`host`, `container:sidecar`, `…`). `None` for value-less flags
	/// (`privileged: true`).
	pub value: Option<String>,
	/// Single-line, render-ready operator message. The `service` field is
	/// already substituted in, so the call site can emit it verbatim.
	pub message: String,
}

/// Scan `service` for every host-binding / privilege-escalation mode it
/// declares and return one [`ModeWarning`] per active mode. `service_name` is
/// the compose key used to label each warning; it is rendered into the
/// message so the call site can emit it verbatim.
///
/// The five compose-native keys — `network_mode`, `privileged`, `pid`, `ipc`,
/// `uts`, `cgroup`, `userns_mode` — are mirrored to one `Namespace` field
/// each on the libpod spec, so the same detector covers the live engine and
/// the Quadlet export. The `container:<id>` prefix on `pid`/`ipc`/`uts`/`cgroup`
/// is treated as host-binding too: sharing another container's namespace
/// collapses isolation the same way `host` does, and podman does not warn on
/// it (the issue scope calls this out as a second, more accurate mode podup
/// should surface).
///
/// Pure so the live engine, the Quadlet path, and the `config` path can call
/// it without threading a Podman client through. Unit tests pin each mode
/// individually and each `container:<id>` arm.
pub fn check_host_mode(service_name: &str, service: &Service) -> Vec<ModeWarning> {
	let mut out = Vec::new();

	if let Some(mode) = service.network_mode.as_deref() {
		if mode == "host" {
			out.push(inherit(service_name, "network_mode", "host"));
		} else if let Some(id) = mode.strip_prefix("container:") {
			out.push(shared_namespace(service_name, "network_mode", id));
		}
	}

	if service.privileged == Some(true) {
		out.push(ModeWarning {
			service: service_name.to_string(),
			field: "privileged",
			value: Some("true".into()),
			message: format!(
				"service \"{service_name}\": privileged: true grants every Linux capability \
				and exposes every host device; under rootless Podman the effect is \
				reduced but the container still bypasses the default capability set"
			),
		});
	}

	attach_namespace(&mut out, service_name, "pid", service.pid.as_deref());
	attach_namespace(&mut out, service_name, "ipc", service.ipc.as_deref());
	attach_namespace(&mut out, service_name, "uts", service.uts.as_deref());
	attach_namespace(&mut out, service_name, "cgroup", service.cgroup.as_deref());

	if let Some(mode) = service.userns_mode.as_deref() {
		if mode == "host" {
			out.push(inherit(service_name, "userns_mode", "host"));
		} else if let Some(id) = mode.strip_prefix("container:") {
			out.push(shared_namespace(service_name, "userns_mode", id));
		}
	}

	out
}

fn inherit(service_name: &str, field: &'static str, mode: &str) -> ModeWarning {
	ModeWarning {
		service: service_name.to_string(),
		field,
		value: Some(mode.to_string()),
		message: format!(
			"service \"{service_name}\": {field}: {mode} shares the host's {label} namespace; \
			the container sees host {subject} and any port it binds is a host port",
			label = label_for(field),
			subject = subject_for(field),
		),
	}
}

fn shared_namespace(service_name: &str, field: &'static str, target: &str) -> ModeWarning {
	ModeWarning {
		service: service_name.to_string(),
		field,
		value: Some(format!("container:{target}")),
		message: format!(
			"service \"{service_name}\": {field}: container:{target} shares another container's \
			{layout} namespace; both containers see the same {subject}",
			layout = label_for(field),
			subject = subject_for(field),
		),
	}
}

fn attach_namespace(
	out: &mut Vec<ModeWarning>,
	service_name: &str,
	field: &'static str,
	value: Option<&str>,
) {
	if let Some(mode) = value {
		if mode == "host" {
			out.push(inherit(service_name, field, mode));
		} else if let Some(id) = mode.strip_prefix("container:") {
			out.push(shared_namespace(service_name, field, id));
		}
	}
}

fn label_for(field: &str) -> &'static str {
	match field {
		"network_mode" => "network",
		"pid" => "PID",
		"ipc" => "IPC",
		"uts" => "UTS",
		"cgroup" => "cgroup",
		"userns_mode" => "user",
		_ => "host",
	}
}

fn subject_for(field: &str) -> &'static str {
	match field {
		"network_mode" => "network interfaces and ports",
		"pid" => "processes (top/ps/wait inspect /proc/<pid>/...)",
		"ipc" => "System V IPC and POSIX message queues",
		"uts" => "hostname and domainname",
		"cgroup" => "cgroup hierarchy and limits",
		"userns_mode" => "UIDs and GIDs (host UID == container UID)",
		_ => "host state",
	}
}

#[cfg(test)]
#[path = "host_mode_tests.rs"]
mod tests;
