use super::*;
use indexmap::IndexMap;

// DependsOn::service_names

#[test]
fn depends_on_empty_has_no_names() {
	assert!(DependsOn::Empty.service_names().is_empty());
}

#[test]
fn restart_policy_serializes_to_compose_string() {
	let ser = |p: &RestartPolicy| serde_yaml::to_string(p).unwrap().trim().to_string();
	assert_eq!(ser(&RestartPolicy::No), "no");
	assert_eq!(ser(&RestartPolicy::Always), "always");
	assert_eq!(ser(&RestartPolicy::UnlessStopped), "unless-stopped");
	assert_eq!(
		ser(&RestartPolicy::OnFailure { max_attempts: None }),
		"on-failure"
	);
	assert_eq!(
		ser(&RestartPolicy::OnFailure {
			max_attempts: Some(5)
		}),
		"on-failure:5"
	);
}

#[test]
fn restart_policy_round_trips_through_yaml() {
	for input in [
		"no",
		"always",
		"unless-stopped",
		"on-failure",
		"on-failure:3",
	] {
		let p: RestartPolicy = serde_yaml::from_str(input).unwrap();
		let out = serde_yaml::to_string(&p).unwrap();
		let reparsed: RestartPolicy = serde_yaml::from_str(&out).unwrap();
		assert_eq!(p, reparsed, "round-trip failed for {input}");
	}
}

#[test]
fn depends_on_list_returns_names() {
	let d = DependsOn::List(vec!["db".into(), "cache".into()]);
	assert_eq!(d.service_names(), vec!["db", "cache"]);
}

#[test]
fn depends_on_map_returns_keys() {
	let mut m = IndexMap::new();
	m.insert(
		"db".to_string(),
		DependsOnCondition {
			condition: ServiceCondition::ServiceHealthy,
			restart: None,
			required: None,
		},
	);
	assert_eq!(DependsOn::Map(m).service_names(), vec!["db"]);
}

// DependsOn::condition_for

#[test]
fn condition_for_empty_defaults_to_started() {
	assert_eq!(
		DependsOn::Empty.condition_for("db"),
		ServiceCondition::ServiceStarted
	);
}

#[test]
fn condition_for_map_returns_explicit() {
	let mut m = IndexMap::new();
	m.insert(
		"db".to_string(),
		DependsOnCondition {
			condition: ServiceCondition::ServiceHealthy,
			restart: None,
			required: None,
		},
	);
	assert_eq!(
		DependsOn::Map(m).condition_for("db"),
		ServiceCondition::ServiceHealthy
	);
}

// DependsOn::restart_for / required_for

#[test]
fn restart_for_list_is_false() {
	assert!(!DependsOn::List(vec!["db".into()]).restart_for("db"));
}

#[test]
fn required_for_list_defaults_true() {
	assert!(DependsOn::List(vec!["db".into()]).required_for("db"));
}

#[test]
fn required_for_map_explicit_false() {
	let mut m = IndexMap::new();
	m.insert(
		"db".to_string(),
		DependsOnCondition {
			condition: ServiceCondition::ServiceStarted,
			restart: None,
			required: Some(false),
		},
	);
	assert!(!DependsOn::Map(m).required_for("db"));
}

// HealthCheck::is_disabled

#[test]
fn healthcheck_disable_true() {
	let hc = HealthCheck {
		disable: Some(true),
		..Default::default()
	};
	assert!(hc.is_disabled());
}

#[test]
fn healthcheck_test_none_exec_disables() {
	let hc = HealthCheck {
		test: Some(Command::Exec(vec!["NONE".to_string()])),
		..Default::default()
	};
	assert!(hc.is_disabled());
}

#[test]
fn healthcheck_real_test_not_disabled() {
	let hc = HealthCheck {
		test: Some(Command::Shell("curl -f http://localhost/".into())),
		..Default::default()
	};
	assert!(!hc.is_disabled());
}

// RestartPolicy deserialization

#[test]
fn restart_policy_no() {
	let p: RestartPolicy = serde_yaml::from_str("\"no\"").unwrap();
	assert_eq!(p, RestartPolicy::No);
}

#[test]
fn restart_policy_always() {
	let p: RestartPolicy = serde_yaml::from_str("\"always\"").unwrap();
	assert_eq!(p, RestartPolicy::Always);
}

#[test]
fn restart_policy_unless_stopped() {
	let p: RestartPolicy = serde_yaml::from_str("\"unless-stopped\"").unwrap();
	assert_eq!(p, RestartPolicy::UnlessStopped);
}

#[test]
fn restart_policy_on_failure_bare() {
	let p: RestartPolicy = serde_yaml::from_str("\"on-failure\"").unwrap();
	assert_eq!(p, RestartPolicy::OnFailure { max_attempts: None });
}

#[test]
fn restart_policy_on_failure_with_count() {
	let p: RestartPolicy = serde_yaml::from_str("\"on-failure:3\"").unwrap();
	assert_eq!(
		p,
		RestartPolicy::OnFailure {
			max_attempts: Some(3)
		}
	);
}

#[test]
fn restart_policy_invalid_is_error() {
	assert!(serde_yaml::from_str::<RestartPolicy>("\"bogus\"").is_err());
}

/// #1095: the extension parses to a typed action.
#[test]
fn podman_on_failure_parses_each_action() {
	for (raw, want) in [
		("none", HealthOnFailure::None),
		("kill", HealthOnFailure::Kill),
		("restart", HealthOnFailure::Restart),
		("stop", HealthOnFailure::Stop),
	] {
		let yaml = format!("test: [\"CMD\", \"true\"]\n{X_PODMAN_ON_FAILURE}: {raw}\n");
		let hc: HealthCheck = serde_yaml::from_str(&yaml).unwrap();
		assert_eq!(hc.podman_on_failure().unwrap(), Some(want), "{raw}");
	}
}

/// A typo is rejected rather than silently leaving a sick container in
/// rotation, the failure this key exists to prevent.
#[test]
fn podman_on_failure_rejects_an_unknown_action() {
	let yaml = format!("test: [\"CMD\", \"true\"]\n{X_PODMAN_ON_FAILURE}: bogus\n");
	let hc: HealthCheck = serde_yaml::from_str(&yaml).unwrap();
	let err = hc
		.podman_on_failure()
		.expect_err("bogus must not be accepted");
	assert!(err.contains("bogus") && err.contains("restart"), "{err}");
}

/// Absent is the ordinary case: no extension, no action, no error.
#[test]
fn podman_on_failure_is_absent_by_default() {
	let hc: HealthCheck = serde_yaml::from_str("test: [\"CMD\", \"true\"]\n").unwrap();
	assert_eq!(hc.podman_on_failure().unwrap(), None);
}

/// The key round-trips through `config`: it lands in `unknown`, which is
/// `#[serde(flatten)]`, so re-serializing the file keeps it. A dropped
/// extension would make `config` output that no longer does what the input
/// did.
#[test]
fn podman_on_failure_survives_a_round_trip() {
	let yaml = format!("test: [\"CMD\", \"true\"]\n{X_PODMAN_ON_FAILURE}: kill\n");
	let hc: HealthCheck = serde_yaml::from_str(&yaml).unwrap();
	let out = serde_yaml::to_string(&hc).unwrap();
	assert!(out.contains(X_PODMAN_ON_FAILURE), "{out}");
	assert!(out.contains("kill"), "{out}");
}
