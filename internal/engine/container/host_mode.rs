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
mod tests {
	use super::check_host_mode;
	use crate::compose::types::{BuildConfig, Service};

	#[test]
	fn a_plain_service_emits_no_warnings() {
		let svc = Service::default();
		assert!(
			check_host_mode("web", &svc).is_empty(),
			"plain service warned"
		);
	}

	#[test]
	fn each_mode_present_triggers_a_warning() {
		for (set, field) in [
			(
				Service {
					network_mode: Some("host".into()),
					..Service::default()
				},
				"network_mode",
			),
			(
				Service {
					privileged: Some(true),
					..Service::default()
				},
				"privileged",
			),
			(
				Service {
					pid: Some("host".into()),
					..Service::default()
				},
				"pid",
			),
			(
				Service {
					ipc: Some("host".into()),
					..Service::default()
				},
				"ipc",
			),
			(
				Service {
					uts: Some("host".into()),
					..Service::default()
				},
				"uts",
			),
			(
				Service {
					cgroup: Some("host".into()),
					..Service::default()
				},
				"cgroup",
			),
			(
				Service {
					userns_mode: Some("host".into()),
					..Service::default()
				},
				"userns_mode",
			),
		] {
			let warnings = check_host_mode("web", &set);
			assert_eq!(
				warnings.len(),
				1,
				"expected exactly one warning for {field}, got {warnings:?}"
			);
			assert_eq!(warnings[0].field, field);
			assert_eq!(warnings[0].service, "web");
			assert_eq!(
				warnings[0].value.as_deref(),
				if field == "privileged" {
					Some("true")
				} else {
					Some("host")
				}
			);
		}
	}

	#[test]
	fn container_namespace_sharing_triggers_a_warning() {
		for (field, make) in [
			(
				"network_mode",
				Box::new(|| Service {
					network_mode: Some("container:sidecar".into()),
					..Service::default()
				}) as Box<dyn Fn() -> Service>,
			),
			(
				"pid",
				Box::new(|| Service {
					pid: Some("container:sidecar".into()),
					..Service::default()
				}),
			),
			(
				"ipc",
				Box::new(|| Service {
					ipc: Some("container:sidecar".into()),
					..Service::default()
				}),
			),
			(
				"uts",
				Box::new(|| Service {
					uts: Some("container:sidecar".into()),
					..Service::default()
				}),
			),
			(
				"cgroup",
				Box::new(|| Service {
					cgroup: Some("container:sidecar".into()),
					..Service::default()
				}),
			),
			(
				"userns_mode",
				Box::new(|| Service {
					userns_mode: Some("container:sidecar".into()),
					..Service::default()
				}),
			),
		] {
			let svc = make();
			let warnings = check_host_mode("web", &svc);
			assert_eq!(
				warnings.len(),
				1,
				"expected one warning for {field}: container:sidecar, got {warnings:?}"
			);
			let w = &warnings[0];
			assert_eq!(w.field, field);
			assert_eq!(w.value.as_deref(), Some("container:sidecar"));
			assert!(
				w.message.contains("container:sidecar"),
				"{field} warning must name the target container: {w:?}"
			);
		}
	}

	#[test]
	fn every_mode_active_at_once_emits_seven_warnings() {
		let svc = Service {
			network_mode: Some("host".into()),
			privileged: Some(true),
			pid: Some("host".into()),
			ipc: Some("host".into()),
			uts: Some("host".into()),
			cgroup: Some("host".into()),
			userns_mode: Some("host".into()),
			image: Some("nginx:1.27".into()),
			..Service::default()
		};
		let warnings = check_host_mode("web", &svc);
		assert_eq!(warnings.len(), 7, "got {warnings:?}");
		let fields: Vec<&str> = warnings.iter().map(|w| w.field).collect();
		assert!(fields.contains(&"network_mode"));
		assert!(fields.contains(&"privileged"));
		assert!(fields.contains(&"pid"));
		assert!(fields.contains(&"ipc"));
		assert!(fields.contains(&"uts"));
		assert!(fields.contains(&"cgroup"));
		assert!(fields.contains(&"userns_mode"));
	}

	#[test]
	fn non_host_values_do_not_warn() {
		for svc in [
			Service {
				network_mode: Some("bridge".into()),
				..Service::default()
			},
			Service {
				network_mode: Some("service:db".into()),
				..Service::default()
			},
			Service {
				network_mode: Some("none".into()),
				..Service::default()
			},
			Service {
				pid: Some("private".into()),
				..Service::default()
			},
			Service {
				uts: Some("shareable".into()),
				..Service::default()
			},
			Service {
				userns_mode: Some("keep-id".into()),
				..Service::default()
			},
		] {
			assert!(
				check_host_mode("web", &svc).is_empty(),
				"non-host value warned on: {svc:?}"
			);
		}
	}

	#[test]
	fn privileged_false_or_absent_does_not_warn() {
		for svc in [
			Service {
				privileged: Some(false),
				..Service::default()
			},
			Service::default(),
		] {
			assert!(check_host_mode("web", &svc).is_empty(), "got: {svc:?}");
		}
	}

	#[test]
	fn message_carries_the_service_name() {
		let svc = Service {
			network_mode: Some("host".into()),
			..Service::default()
		};
		let warnings = check_host_mode("api", &svc);
		assert_eq!(warnings.len(), 1);
		assert!(
			warnings[0].message.contains("service \"api\""),
			"message must carry the service name: {warnings:?}"
		);
	}

	#[test]
	fn value_carries_the_verbatim_compose_value() {
		let svc = Service {
			ipc: Some("container:guest-app".into()),
			..Service::default()
		};
		let warnings = check_host_mode("web", &svc);
		assert_eq!(warnings.len(), 1);
		assert_eq!(warnings[0].value.as_deref(), Some("container:guest-app"));
	}

	#[test]
	fn build_only_service_resolves_a_name() {
		let svc = Service {
			build: Some(BuildConfig::Context(".".into())),
			network_mode: Some("host".into()),
			..Service::default()
		};
		let warnings = check_host_mode("worker", &svc);
		assert_eq!(warnings.len(), 1);
		assert!(warnings[0].message.contains("service \"worker\""));
	}
}
