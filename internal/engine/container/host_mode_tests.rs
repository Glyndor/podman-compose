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
