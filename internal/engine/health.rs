//! Health and completion polling for service dependency ordering.
//!
//! [`Engine::wait_healthy`] polls until the container reports `healthy` (used when
//! a dependent service declares `condition: service_healthy`).
//! [`Engine::wait_completed`] polls until the container exits with code 0 (used for
//! `condition: service_completed_successfully`).

use std::time::Duration;

use serde::Deserialize;

use crate::compose::types::{ComposeFile, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::ContainerInspect;
use crate::libpod::API_PREFIX;

use super::Engine;

/// Response from `GET {API_PREFIX}/containers/{name}/healthcheck`, which *runs*
/// the container's healthcheck on demand and reports the resulting status.
#[derive(Deserialize)]
struct HealthCheckRun {
	#[serde(rename = "Status")]
	status: Option<String>,
}

/// Per-poll verdict while waiting for `service_healthy`.
enum HealthVerdict {
	/// The runtime reports the container as `healthy`.
	Healthy,
	/// The container has no effective healthcheck, so `healthy` is unreachable;
	/// treat the dependency as satisfied rather than blocking until timeout.
	NoHealthcheck,
	/// The container has already exited non-zero. It can never become healthy and
	/// the service failed to start, so the wait must fail rather than report the
	/// dependency satisfied. Carries the exit code.
	Failed(i64),
	/// Not healthy yet; keep polling.
	Pending,
}

/// Classify a container inspect while waiting for `service_healthy`.
///
/// Pure decision logic for [`Engine::wait_healthy`], split out so the gating
/// behaviour can be unit-tested without a live Podman socket.
fn classify_health(info: &ContainerInspect) -> HealthVerdict {
	if let Some(state) = &info.state {
		if let Some(health) = &state.health {
			if health.status.as_deref() == Some("healthy") {
				return HealthVerdict::Healthy;
			}
		}
		// A container that has already exited can never become healthy. If it
		// exited non-zero the service failed to start, so fail the wait instead of
		// reporting it satisfied (which would mask the failure in `up --wait`/CI).
		// An exit code of 0 is a one-shot that completed; let it fall through.
		if state.status.as_deref() == Some("exited") {
			let code = state.exit_code.unwrap_or(0);
			if code != 0 {
				return HealthVerdict::Failed(code);
			}
		}
	}
	if !info
		.config
		.as_ref()
		.map(|c| c.has_healthcheck())
		.unwrap_or(false)
	{
		return HealthVerdict::NoHealthcheck;
	}
	HealthVerdict::Pending
}

/// How often the health *status* is read while waiting between check runs.
///
/// Reading is a plain inspect: it does not execute the healthcheck, so it costs
/// a request and nothing inside the container. Podman runs the check on its own
/// schedule where systemd is available, and this is what notices promptly when
/// it does: previously the status was only ever looked at once per `interval`,
/// so a container that turned healthy just after a probe went unnoticed for the
/// rest of the window.
const STATUS_READ_INTERVAL: Duration = Duration::from_millis(150);

/// Lower bound on how often podup will *run* a healthcheck.
///
/// Running is not free (it executes the command inside the container) so an
/// `interval` of `10ms` must not turn into a hundred executions a second.
const MIN_RUN_INTERVAL: Duration = Duration::from_millis(100);

/// Compute the `(run_interval, budget)` for [`Engine::wait_healthy`] from a
/// healthcheck's `interval`/`start_period`/`retries`.
///
/// `run_interval` is how often podup actively executes the check; `budget` is
/// how long it keeps trying. Sub-second intervals are honoured down to
/// [`MIN_RUN_INTERVAL`]: they used to be discarded and replaced by the 2s
/// default, so asking for `500ms` polling produced *slower* polling than asking
/// for `1s`, the opposite of the request. Pure so the timing is unit-testable.
fn health_poll_plan(
	interval: Option<&str>,
	start_period: Option<&str>,
	retries: Option<u32>,
) -> (Duration, Duration) {
	let run_interval = interval
		.and_then(crate::size::parse_duration_nanos)
		.filter(|n| *n > 0)
		.map(|n| Duration::from_nanos(n as u64).max(MIN_RUN_INTERVAL))
		.unwrap_or(Duration::from_secs(2));
	let start = start_period
		.and_then(crate::size::parse_duration_nanos)
		.filter(|n| *n > 0)
		.map(|n| Duration::from_nanos(n as u64))
		.unwrap_or_default();
	// Same budget as before, expressed as time rather than as a probe count: a
	// count stopped meaning a duration once the wait polls at two cadences.
	let budget = run_interval.saturating_mul(retries.unwrap_or(30)) + start;
	(run_interval, budget)
}

/// Poll iterations to actually run, given the healthcheck poll-plan and an
/// optional `--wait-timeout`.
///
/// `--wait-timeout` must be able to *extend* the wait, not merely cap it: a
/// healthcheck with a short `interval × retries` budget would otherwise time
/// out long before a generous `--wait-timeout` elapsed. So when a wait-timeout
/// is given we run at least enough iterations to cover it (plus a small margin),
/// letting the outer `--wait-timeout` deadline, not the poll plan, decide when
/// to give up. Without a wait-timeout the poll plan governs unchanged. Pure so
/// the budget arithmetic is unit-tested without a live socket.
fn effective_budget(
	run_interval: Duration,
	plan_budget: Duration,
	wait_timeout: Option<Duration>,
) -> Duration {
	match wait_timeout {
		// Two extra run intervals so this wait outlasts the outer deadline,
		// ensuring an exhausted wait surfaces as the `--wait-timeout` error
		// rather than a spurious health-check timeout that fired just early.
		Some(wt) => plan_budget.max(wt + run_interval.saturating_mul(2)),
		None => plan_budget,
	}
}

impl Engine {
	/// Wait until every targeted service's first replica is healthy (`up
	/// --wait`). A service with no effective healthcheck is treated as ready
	/// once started. All services when `target_services` is empty.
	pub async fn wait_services_healthy(
		&self,
		file: &ComposeFile,
		target_services: &[String],
	) -> Result<()> {
		self.wait_services_healthy_within(file, target_services, None)
			.await
	}

	/// As [`wait_services_healthy`](Self::wait_services_healthy), but a
	/// `Some(wait_timeout)` extends each service's poll budget to cover the
	/// supplied `--wait-timeout` (rather than only capping it). The caller still
	/// wraps the whole wait in a hard `--wait-timeout` deadline, which becomes the
	/// authoritative limit; this just stops the per-service poll plan from giving
	/// up early and reporting a misleading health-check timeout.
	pub async fn wait_services_healthy_within(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		wait_timeout: Option<Duration>,
	) -> Result<()> {
		// Poll services concurrently: each `wait_healthy` is its own poll loop, so
		// total `--wait` latency is the slowest service, not the sum of all.
		let waits = file
			.services
			.iter()
			.filter(|(name, _)| {
				target_services.is_empty() || target_services.iter().any(|t| t == *name)
			})
			.map(|(name, service)| {
				let container = self.first_replica_name(name, service);
				async move { self.wait_healthy(&container, service, wait_timeout).await }
			});
		futures_util::future::try_join_all(waits).await?;
		Ok(())
	}

	/// Poll a container until its health status is `healthy` or timeout.
	///
	/// Polls at the compose `healthcheck.interval` (default 2 s) for
	/// `healthcheck.retries` (default 30) probes, plus extra probes covering
	/// `healthcheck.start_period` so a slow-starting service is not timed out early.
	///
	/// The wait is driven by the container's *effective* healthcheck reported by
	/// the runtime, so healthchecks inherited from the image count too, not just
	/// those declared in compose. If the container has no effective healthcheck at
	/// all (none in the image or compose), it can never report `healthy`, so the
	/// wait short-circuits as satisfied rather than blocking until timeout.
	pub(super) async fn wait_healthy(
		&self,
		container_name: &str,
		service: &Service,
		wait_timeout: Option<Duration>,
	) -> Result<()> {
		let hc = service.healthcheck.as_ref();
		let (run_interval, plan_budget) = health_poll_plan(
			hc.and_then(|h| h.interval.as_deref()),
			hc.and_then(|h| h.start_period.as_deref()),
			hc.and_then(|h| h.retries),
		);
		// `--wait-timeout` extends the budget so it, not the (often shorter)
		// healthcheck interval×retries plan, decides when the wait gives up.
		let budget = effective_budget(run_interval, plan_budget, wait_timeout);

		// One inspect decides the short-circuits: already healthy, or no effective
		// healthcheck at all (image or compose), in which case a server-side
		// `wait?condition=healthy` would block forever, so treat it as satisfied.
		let info = self
			.client
			.get_json::<crate::libpod::types::container::ContainerInspect>(&format!(
				"{API_PREFIX}/containers/{}/json",
				crate::libpod::urlencoded(container_name),
			))
			.await
			.map_err(ComposeError::Podman)?;
		match classify_health(&info) {
			HealthVerdict::Healthy => return Ok(()),
			HealthVerdict::NoHealthcheck => {
				tracing::debug!(
					"{container_name} has no effective healthcheck; treating service_healthy as satisfied"
				);
				return Ok(());
			}
			HealthVerdict::Failed(code) => {
				return Err(ComposeError::WaitServiceExited {
					container: container_name.to_string(),
					code,
				});
			}
			HealthVerdict::Pending => {}
		}

		// Actively drive the healthcheck on demand. A server-side
		// `wait?condition=healthy` only returns once the health *status* flips to
		// `healthy`, but Podman updates that status only when the healthcheck runs
		// and it schedules those runs via systemd transient timers. Without
		// systemd (containers, minimal hosts) the timer never fires and the status
		// stays `starting`, so the wait would block until the whole budget elapsed.
		//
		// `GET {API_PREFIX}/containers/{name}/healthcheck` *runs* the check and
		// returns the resulting status, so polling it works with or without
		// systemd, on Podman 4.9.3 and 5.x alike. Extra on-demand runs on a
		// systemd host are harmless.
		let path = format!(
			"{API_PREFIX}/containers/{}/healthcheck",
			crate::libpod::urlencoded(container_name),
		);
		// Two cadences, because running a check and observing its result are not
		// the same cost. Running executes a command inside the container, so it
		// happens at the interval the compose file asked for and no faster.
		// Observing is a plain inspect, so it happens often: Podman runs the
		// check on its own systemd schedule where one exists, and this is what
		// notices promptly when it does.
		//
		// Before, the status was read only once per run, so a container that
		// turned healthy a moment after a probe went unseen for the rest of the
		// interval: measured at 2507ms to notice a service healthy at ~1200ms,
		// against 1706ms for docker compose (#1147).
		let inspect_path = format!(
			"{API_PREFIX}/containers/{}/json",
			crate::libpod::urlencoded(container_name),
		);
		let deadline = tokio::time::Instant::now() + budget;
		let mut next_run = tokio::time::Instant::now();
		while tokio::time::Instant::now() < deadline {
			if tokio::time::Instant::now() >= next_run {
				match self.client.get_json::<HealthCheckRun>(&path).await {
					Ok(run) if run.status.as_deref() == Some("healthy") => return Ok(()),
					Ok(_) => {}
					// A transient error (container not yet running, 409, 500, …)
					// just means "not healthy yet", so keep going rather than
					// failing hard.
					Err(e) => tracing::debug!("{container_name} healthcheck run failed: {e}"),
				}
				next_run = tokio::time::Instant::now() + run_interval;
			} else if let Ok(info) = self
				.client
				.get_json::<crate::libpod::types::container::ContainerInspect>(&inspect_path)
				.await
			{
				match classify_health(&info) {
					HealthVerdict::Healthy => return Ok(()),
					HealthVerdict::Failed(code) => {
						return Err(ComposeError::WaitServiceExited {
							container: container_name.to_string(),
							code,
						})
					}
					_ => {}
				}
			}
			let nap = STATUS_READ_INTERVAL.min(run_interval);
			tokio::time::sleep(
				nap.min(deadline.saturating_duration_since(tokio::time::Instant::now())),
			)
			.await;
		}
		Err(ComposeError::HealthCheckTimeout(container_name.into()))
	}

	/// Wait until a container exits, then require status 0.
	///
	/// Blocks server-side on `wait?condition=stopped` (which returns the exit
	/// code) instead of polling inspect, bounded by a 600 s client-side timeout.
	/// Errors if the container exits non-zero or the deadline is exceeded.
	pub(super) async fn wait_completed(&self, container_name: &str) -> Result<()> {
		let path = format!(
			"{API_PREFIX}/containers/{}/wait?condition=stopped",
			crate::libpod::urlencoded(container_name),
		);
		let budget = std::time::Duration::from_secs(600);
		match tokio::time::timeout(budget, self.client.post_empty_json_unbounded::<i64>(&path))
			.await
		{
			Ok(Ok(0)) => Ok(()),
			Ok(Ok(code)) => Err(ComposeError::HealthCheckTimeout(format!(
				"{container_name} exited with non-zero status {code}"
			))),
			Ok(Err(e)) => {
				tracing::debug!("{container_name} wait?condition=stopped failed: {e}");
				Err(ComposeError::HealthCheckTimeout(container_name.into()))
			}
			Err(_elapsed) => Err(ComposeError::HealthCheckTimeout(container_name.into())),
		}
	}
}

#[cfg(test)]
#[path = "health_tests.rs"]
mod tests;
