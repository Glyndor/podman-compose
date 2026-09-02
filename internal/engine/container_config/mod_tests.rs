use super::*;
use crate::compose::types::{
	Command as ComposeCommand, HealthCheck, RestartPolicy as ComposeRestart, Service,
};

fn default_service() -> Service {
	Service::default()
}

// --- restart policy ---

#[test]
fn restart_policy_no() {
	let mut svc = default_service();
	svc.restart = Some(ComposeRestart::No);
	let (name, tries) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("no"));
	assert!(tries.is_none());
}

#[test]
fn restart_policy_always() {
	let mut svc = default_service();
	svc.restart = Some(ComposeRestart::Always);
	let (name, _) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("always"));
}

#[test]
fn restart_policy_on_failure_with_retries() {
	let mut svc = default_service();
	svc.restart = Some(ComposeRestart::OnFailure {
		max_attempts: Some(3),
	});
	let (name, tries) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("on-failure"));
	assert_eq!(tries, Some(3));
}

#[test]
fn restart_policy_unless_stopped() {
	let mut svc = default_service();
	svc.restart = Some(ComposeRestart::UnlessStopped);
	let (name, _) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("unless-stopped"));
}

#[test]
fn restart_policy_none_when_absent() {
	let (name, _) = build_restart_policy(&default_service());
	assert!(name.is_none());
}

#[test]
fn restart_policy_from_deploy_on_failure() {
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy {
			condition: Some("on-failure".into()),
			max_attempts: Some(5),
			..Default::default()
		}),
		..Default::default()
	});
	let (name, tries) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("on-failure"));
	assert_eq!(tries, Some(5));
}

#[test]
fn restart_policy_from_deploy_none_condition() {
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy {
			condition: Some("none".into()),
			..Default::default()
		}),
		..Default::default()
	});
	let (name, _) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("no"));
}

#[test]
fn restart_policy_from_deploy_any_maps_to_always() {
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy {
			condition: Some("any".into()),
			..Default::default()
		}),
		..Default::default()
	});
	let (name, _) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("always"));
}

#[test]
fn restart_policy_from_deploy_default_condition_is_always() {
	// An unset `condition` defaults to `any` per the compose spec → `always`.
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy::default()),
		..Default::default()
	});
	let (name, _) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("always"));
}

#[test]
fn restart_policy_from_deploy_any_drops_max_attempts() {
	// `condition: any` maps to `always`; Podman ignores a retry cap under
	// `always`, so we must not forward max_attempts (it would otherwise read
	// as an honoured bound that the backend silently discards).
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy {
			condition: Some("any".into()),
			max_attempts: Some(3),
			..Default::default()
		}),
		..Default::default()
	});
	let (name, tries) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("always"));
	assert!(tries.is_none());
}

#[test]
fn restart_policy_from_deploy_none_drops_max_attempts() {
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy {
			condition: Some("none".into()),
			max_attempts: Some(4),
			..Default::default()
		}),
		..Default::default()
	});
	let (name, tries) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("no"));
	assert!(tries.is_none());
}

#[test]
fn restart_policy_from_deploy_unrecognized_condition_drops_max_attempts() {
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy {
			condition: Some("on-success".into()),
			max_attempts: Some(2),
			..Default::default()
		}),
		..Default::default()
	});
	let (name, tries) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("unless-stopped"));
	assert!(tries.is_none());
}

#[test]
fn restart_policy_from_deploy_on_failure_keeps_max_attempts() {
	// The one condition that honours the cap must still forward it.
	use crate::compose::types::{DeployConfig, DeployRestartPolicy};
	let mut svc = default_service();
	svc.deploy = Some(DeployConfig {
		restart_policy: Some(DeployRestartPolicy {
			condition: Some("on-failure".into()),
			max_attempts: Some(3),
			..Default::default()
		}),
		..Default::default()
	});
	let (name, tries) = build_restart_policy(&svc);
	assert_eq!(name.as_deref(), Some("on-failure"));
	assert_eq!(tries, Some(3));
}

// --- healthcheck ---

#[test]
fn healthcheck_disabled() {
	let hc = HealthCheck {
		disable: Some(true),
		..Default::default()
	};
	let cfg = build_healthcheck(&hc);
	assert_eq!(cfg.test.unwrap(), vec!["NONE"]);
}

#[test]
fn healthcheck_shell_command() {
	let hc = HealthCheck {
		test: Some(ComposeCommand::Shell(
			"curl -f http://localhost/health".into(),
		)),
		interval: Some("30s".into()),
		timeout: Some("10s".into()),
		retries: Some(3),
		..Default::default()
	};
	let cfg = build_healthcheck(&hc);
	let test = cfg.test.unwrap();
	assert_eq!(test[0], "CMD-SHELL");
	assert!(test[1].contains("curl"));
	assert_eq!(cfg.retries, Some(3));
}

#[test]
fn healthcheck_exec_command() {
	let hc = HealthCheck {
		test: Some(ComposeCommand::Exec(vec![
			"curl".into(),
			"-f".into(),
			"http://localhost".into(),
		])),
		..Default::default()
	};
	let cfg = build_healthcheck(&hc);
	let test = cfg.test.unwrap();
	assert_eq!(test[0], "curl");
}

#[test]
fn healthcheck_applies_compose_defaults_when_omitted() {
	// A healthcheck with only a `test` must still get interval/timeout/retries:
	// Podman treats a missing Timeout as 0s, which makes every probe fail with
	// "exceeded timeout of 0s" and the container is stuck `starting`.
	let hc = HealthCheck {
		test: Some(ComposeCommand::Exec(vec!["true".into()])),
		..Default::default()
	};
	let cfg = build_healthcheck(&hc);
	assert_eq!(cfg.interval, Some(30 * 1_000_000_000));
	assert_eq!(cfg.timeout, Some(30 * 1_000_000_000));
	assert_eq!(cfg.retries, Some(3));
}

#[test]
fn healthcheck_honors_explicit_interval_and_timeout() {
	let hc = HealthCheck {
		test: Some(ComposeCommand::Exec(vec!["true".into()])),
		interval: Some("2s".into()),
		timeout: Some("5s".into()),
		retries: Some(7),
		..Default::default()
	};
	let cfg = build_healthcheck(&hc);
	assert_eq!(cfg.interval, Some(2 * 1_000_000_000));
	assert_eq!(cfg.timeout, Some(5 * 1_000_000_000));
	assert_eq!(cfg.retries, Some(7));
}
