use super::{
	HealthCheckOnFailureAction, HealthConfig, LinuxDeviceCgroup, SpecGenerator, StartupHealthCheck,
	Ulimit,
};

#[test]
fn security_fields_serialize_decomposed_not_as_security_opt() {
	let spec = SpecGenerator {
		selinux_opts: vec!["disable".to_string()],
		apparmor_profile: Some("prof".to_string()),
		seccomp_profile_path: Some("unconfined".to_string()),
		no_new_privileges: Some(true),
		mask: vec!["/proc/kcore".to_string()],
		unmask: vec!["ALL".to_string()],
		..Default::default()
	};
	let v = serde_json::to_value(&spec).unwrap();
	// SpecGenerator has no `security_opt` field, so the value must arrive decomposed.
	assert!(
		v.get("security_opt").is_none(),
		"stale security_opt key: {v}"
	);
	assert_eq!(v["selinux_opts"][0], "disable");
	assert_eq!(v["apparmor_profile"], "prof");
	assert_eq!(v["seccomp_profile_path"], "unconfined");
	assert_eq!(v["no_new_privileges"], true);
	assert_eq!(v["mask"][0], "/proc/kcore");
	assert_eq!(v["unmask"][0], "ALL");
}

#[test]
fn device_cgroup_rule_serializes_as_struct_array() {
	let spec = SpecGenerator {
		device_cgroup_rule: vec![LinuxDeviceCgroup {
			allow: true,
			device_type: Some("c".to_string()),
			major: Some(1),
			minor: None,
			access: Some("rwm".to_string()),
		}],
		..Default::default()
	};
	let v = serde_json::to_value(&spec).unwrap();
	// Podman expects []LinuxDeviceCgroup objects, not strings.
	assert_eq!(v["device_cgroup_rule"][0]["allow"], true);
	assert_eq!(v["device_cgroup_rule"][0]["type"], "c");
	assert_eq!(v["device_cgroup_rule"][0]["major"], 1);
	// minor=None must be omitted (means "all").
	assert!(v["device_cgroup_rule"][0].get("minor").is_none());
	assert_eq!(v["device_cgroup_rule"][0]["access"], "rwm");
}

#[test]
fn no_cdi_devices_key_is_emitted() {
	// Podman 5.x has no cdi_devices field; CDI names ride in `devices`.
	let v = serde_json::to_value(SpecGenerator::default()).unwrap();
	assert!(v.get("cdi_devices").is_none(), "stale cdi_devices key: {v}");
}

#[test]
fn extra_hosts_serialize_as_hostadd() {
	let spec = SpecGenerator {
		extra_hosts: vec!["db:10.0.0.2".to_string()],
		..Default::default()
	};
	let v = serde_json::to_value(&spec).unwrap();
	// Podman's SpecGenerator key is `hostadd`; `extra_hosts` matches no field
	// and is silently dropped.
	assert_eq!(v["hostadd"][0], "db:10.0.0.2");
	assert!(v.get("extra_hosts").is_none(), "stale extra_hosts key: {v}");
}

#[test]
fn ulimits_serialize_as_r_limits_with_posix_shape() {
	let spec = SpecGenerator {
		ulimits: vec![Ulimit {
			ulimit_type: "nofile".to_string(),
			soft: 1024,
			hard: 2048,
		}],
		..Default::default()
	};
	let v = serde_json::to_value(&spec).unwrap();
	// Podman's key is `r_limits`; the element shape is POSIXRlimit {type, soft, hard}.
	assert!(v.get("ulimits").is_none(), "stale ulimits key: {v}");
	assert_eq!(v["r_limits"][0]["type"], "nofile");
	assert_eq!(v["r_limits"][0]["soft"], 1024);
	assert_eq!(v["r_limits"][0]["hard"], 2048);
}

#[test]
fn health_on_failure_and_startup_use_podman_wire_names() {
	let spec = SpecGenerator {
		health_check_on_failure_action: Some(HealthCheckOnFailureAction::Restart),
		startup_health_config: Some(StartupHealthCheck {
			health_config: HealthConfig {
				test: Some(vec!["CMD".to_string(), "true".to_string()]),
				interval: Some(1_000_000_000),
				..Default::default()
			},
			successes: Some(3),
		}),
		..Default::default()
	};
	let v = serde_json::to_value(&spec).unwrap();

	// `--health-on-failure` rides as Podman's integer action code (restart = 3),
	// under the snake_case key, not as a string and not as `none`(0).
	assert_eq!(v["health_check_on_failure_action"], 3);

	// The startup probe nests under the PascalCase `startupHealthConfig` key,
	// with its embedded probe fields flattened (PascalCase) and `Successes`.
	let startup = &v["startupHealthConfig"];
	assert_eq!(startup["Test"][0], "CMD");
	assert_eq!(startup["Test"][1], "true");
	assert_eq!(startup["Interval"], 1_000_000_000_i64);
	assert_eq!(startup["Successes"], 3);
	// Flattened: there is no nested `health_config` wrapper key.
	assert!(startup.get("health_config").is_none(), "not flattened: {v}");
}

#[test]
fn health_fields_omitted_when_unset() {
	// Both new fields are `Option` and must vanish from the wire when unset.
	let v = serde_json::to_value(SpecGenerator::default()).unwrap();
	assert!(v.get("health_check_on_failure_action").is_none());
	assert!(v.get("startupHealthConfig").is_none());
}

#[test]
fn on_failure_action_serializes_to_podman_integers() {
	// Podman assigns each action a non-contiguous integer; a wrong discriminant
	// would silently mis-drive the container, so pin every variant's wire value.
	use super::HealthCheckOnFailureAction as A;
	for (action, wire) in [(A::None, 0), (A::Kill, 2), (A::Restart, 3), (A::Stop, 4)] {
		let v = serde_json::to_value(action).unwrap();
		assert_eq!(v, wire, "{action:?} should serialize to {wire}");
	}
}
