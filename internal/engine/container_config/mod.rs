//! Pure configuration builders for container creation: restart policy, logging,
//! healthcheck, resource limits, and ulimits.
//!
//! Device, blkio, tmpfs, and label-file helpers live in `super::container::fields`.

use std::collections::HashMap;

use crate::compose::types::{
	Command as ComposeCommand, HealthCheck, LoggingConfig, RestartPolicy as ComposeRestart, Service,
};
use crate::error::ComposeError;
use crate::libpod::types::container::{HealthConfig, LogConfig};
use crate::size;

pub(crate) mod resources;
pub(super) use resources::{build_resource_limits, build_ulimits, cdi_devices};

// ---------------------------------------------------------------------------
// Restart policy
// ---------------------------------------------------------------------------

/// Returns `(policy_name, max_retry_tries)` for SpecGenerator.
pub(super) fn build_restart_policy(service: &Service) -> (Option<String>, Option<u64>) {
	if let Some(r) = &service.restart {
		let (name, tries) = match r {
			ComposeRestart::No => ("no", None),
			ComposeRestart::Always => ("always", None),
			ComposeRestart::OnFailure { max_attempts } => {
				("on-failure", max_attempts.map(|n| n as u64))
			}
			ComposeRestart::UnlessStopped => ("unless-stopped", None),
		};
		return (Some(name.to_string()), tries);
	}
	if let Some(drp) = service
		.deploy
		.as_ref()
		.and_then(|d| d.restart_policy.as_ref())
	{
		// Compose `restart_policy.condition`: `any` (the default) means restart
		// under any circumstance, which docker-compose maps to `always` — not
		// `unless-stopped` (the latter would skip restarts after an explicit
		// stop, diverging from docker-compose).
		let name = match drp.condition.as_deref().unwrap_or("any") {
			"none" => "no",
			"on-failure" => "on-failure",
			"any" => "always",
			other => {
				tracing::warn!(
					"deploy.restart_policy.condition '{other}' is not recognized \
					 (expected none/on-failure/any); falling back to 'unless-stopped'"
				);
				"unless-stopped"
			}
		};
		// Podman only honours a retry cap (`RestartRetries`) when the policy is
		// `on-failure`. Under any other policy the cap is silently dropped by the
		// backend, which would turn a bounded "restart at most N times" spec into an
		// unbounded restart. Only forward `max_attempts` for `on-failure`, and warn
		// if the user set one under a condition where it cannot take effect.
		let tries = if name == "on-failure" {
			drp.max_attempts.map(|n| n as u64)
		} else {
			if drp.max_attempts.is_some() {
				tracing::warn!(
					"deploy.restart_policy.max_attempts is ignored unless condition \
					 is 'on-failure' (current condition resolves to '{name}')"
				);
			}
			None
		};
		return (Some(name.to_string()), tries);
	}
	(None, None)
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// Default log rotation applied when a compose file does not carry a
/// `logging:` block. `k8s-file` is the libpod default since Podman 4; pinning
/// it explicitly here stops the answer from drifting between podman versions
/// and distros. The 10 MB cap is enough for a week of typical service output
/// and small enough that a runaway loop will not exhaust the host. libpod
/// does not honour `max-file` on any path (see `man podman-run`), so it is
/// not part of the default; the user can override by writing their own
/// `logging:` in compose (#1417).
pub(crate) fn default_log_config() -> LogConfig {
	LogConfig {
		driver: Some("k8s-file".into()),
		size: Some(10 * 1024 * 1024),
		options: HashMap::new(),
	}
}

/// Resolve the `logging:` block into libpod's `LogConfig`. An absent compose
/// `logging:` maps to [`default_log_config`] so every container podup creates
/// has a rotation policy without the user having to set one. When the user
/// supplies a block, `max-size` is parsed into the typed [`LogConfig::size`]
/// field libpod actually reads rotation from (passing it inside `options` is
/// silently ignored — #1417); `max-file` is dropped with a warning because
/// libpod does not implement it. A malformed `max-size` is rejected with a
/// `PodmanError::Field` so the user gets a compose-flavoured error instead of
/// a 500 from libpod's JSON unmarshal.
pub(crate) fn build_log_config(
	service_name: &str,
	logging: Option<&LoggingConfig>,
) -> Result<Option<LogConfig>, ComposeError> {
	match logging {
		Some(l) => Ok(Some(translate_user_logging(service_name, l)?)),
		None => Ok(Some(default_log_config())),
	}
}

/// Translate a user-supplied `logging:` block into libpod's [`LogConfig`].
///
/// `max-size` is moved into the typed `size` field. `max-file` is dropped
/// with a warning — libpod does not implement it and would silently ignore
/// it if forwarded. The user-supplied value of `max-size` is parsed with the
/// same memory parser used elsewhere in the engine (`10m`, `1g`, plain
/// bytes); a malformed value is rejected with the service field name so the
/// error points at the compose key the user wrote (#1417).
fn translate_user_logging(
	service_name: &str,
	l: &LoggingConfig,
) -> Result<LogConfig, ComposeError> {
	let mut options = l.options.clone();
	let size = match options.remove("max-size") {
		Some(v) => match size::parse_memory(&v) {
			Some(bytes) => Some(bytes),
			None => {
				return Err(ComposeError::Podman(
					crate::libpod::validate::spec_field_error(
						service_name,
						"logging.options.max-size",
						&v,
						"must be a byte count (e.g. '10m', '1024', '1g'); \
						 libpod rejects an invalid `size` outright",
					),
				));
			}
		},
		None => None,
	};
	if options.remove("max-file").is_some() {
		tracing::warn!(
			"logging.options.max-file is ignored by libpod; \
			 remove it from your compose file or expect unbounded log growth"
		);
	}
	Ok(LogConfig {
		driver: l.driver.clone(),
		size,
		options,
	})
}

// ---------------------------------------------------------------------------
// Healthcheck
// ---------------------------------------------------------------------------

pub(super) fn build_healthcheck(hc: &HealthCheck) -> HealthConfig {
	if hc.is_disabled() {
		return HealthConfig {
			test: Some(vec!["NONE".to_string()]),
			..Default::default()
		};
	}
	let test = hc.test.as_ref().map(|cmd| match cmd {
		ComposeCommand::Shell(s) => vec!["CMD-SHELL".to_string(), s.clone()],
		ComposeCommand::Exec(v) => v.clone(),
	});
	// Apply the compose-spec defaults for any field the user omitted. Podman's
	// API does NOT default these: a missing `Timeout` is taken as 0s, which makes
	// every probe fail with "exceeded timeout of 0s" so the container is stuck
	// `starting`; a missing/zero `Interval` disables the periodic check. Match
	// docker-compose — interval 30s, timeout 30s, retries 3 (start_period 0).
	const DEFAULT_NANOS: i64 = 30 * 1_000_000_000;
	HealthConfig {
		test,
		interval: Some(
			hc.interval
				.as_deref()
				.and_then(size::parse_duration_nanos)
				.unwrap_or(DEFAULT_NANOS),
		),
		timeout: Some(
			hc.timeout
				.as_deref()
				.and_then(size::parse_duration_nanos)
				.unwrap_or(DEFAULT_NANOS),
		),
		retries: Some(hc.retries.map(|r| r as i64).unwrap_or(3)),
		start_period: hc
			.start_period
			.as_deref()
			.and_then(size::parse_duration_nanos),
		start_interval: hc
			.start_interval
			.as_deref()
			.and_then(size::parse_duration_nanos),
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "log_config_tests.rs"]
mod log_config_tests;
