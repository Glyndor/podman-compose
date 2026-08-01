//! The `scale` subcommand plus the replica-listing/reconciliation helpers
//! shared with teardown.

use std::collections::HashSet;

use crate::compose::types::{ComposeFile, Service};
use crate::engine::Engine;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::parallel::first_error;
use super::targets::{stop_deadline, stop_timeout_param};

/// A live project container: the name to act on plus its machine-readable
/// state, as reported by the libpod container-list endpoint. Returned by
/// [`Engine::live_service_containers`] so callers can act on (and report) only
/// containers in the right state.
pub(crate) struct LiveContainer {
	/// Container name with the leading slash stripped.
	pub name: String,
	/// Machine-readable state (`running`, `created`, `exited`, `paused`, …).
	pub state: String,
}

/// Whether a container in this state is currently active — i.e. `stop` would
/// actually transition it. A `running` or `paused` container is stopped; a
/// `created`/`exited`/`dead`/… one is already not running, so stopping it is a
/// no-op that must not be reported as "stopped" (#876). Pure for unit testing.
pub(crate) fn state_is_active(state: &str) -> bool {
	matches!(state, "running" | "paused")
}

/// The default ceiling on a service's replica count.
const DEFAULT_MAX_REPLICAS: u32 = 256;

/// The replica ceiling, overridable via the `PODUP_MAX_REPLICAS` environment
/// variable (a host operator's escape hatch). A missing, unparseable, or zero
/// override falls back to [`DEFAULT_MAX_REPLICAS`].
fn max_replicas() -> u32 {
	std::env::var("PODUP_MAX_REPLICAS")
		.ok()
		.and_then(|v| v.parse::<u32>().ok())
		.filter(|&n| n > 0)
		.unwrap_or(DEFAULT_MAX_REPLICAS)
}

/// Reject a replica count beyond the configured ceiling. Guards both the CLI
/// `scale`/`--scale` path and an untrusted compose `deploy.replicas`/`scale:`
/// from driving podup into unbounded container creation (a host DoS), since
/// every command resolves its replica count through this one check.
pub(super) fn check_replica_limit(service_name: &str, replicas: usize) -> Result<()> {
	let max = max_replicas();
	if replicas as u64 > u64::from(max) {
		return Err(ComposeError::ReplicaLimitExceeded {
			service: service_name.to_string(),
			replicas,
			max,
		});
	}
	Ok(())
}

/// Reject a scaled service that publishes a fixed host port: only one container
/// can bind a given host port, so replicas 2..N would fail at runtime with
/// `address already in use`. A host port of 0/None is runtime-assigned by
/// Podman, so such a service scales fine. The compose-spec does not define how
/// scaling interacts with published ports, so podup fails fast rather than
/// inventing surprising auto-offset semantics.
pub(super) fn check_scale_port_conflict(
	service_name: &str,
	service: &Service,
	replicas: usize,
) -> Result<()> {
	if replicas <= 1 {
		return Ok(());
	}
	let fixed: Vec<u16> = crate::ports::parse_ports(&service.ports)?
		.iter()
		.filter_map(|p| p.host_port)
		.filter(|&hp| hp != 0)
		.collect();
	if fixed.is_empty() {
		return Ok(());
	}
	Err(ComposeError::ScalePortConflict {
		service: service_name.to_string(),
		replicas,
		ports: fixed,
	})
}

/// Reject scaling a service that pins an explicit `container_name` above one
/// replica. A fixed container name can only ever name a single container, so
/// inventing `name-1`, `name-2`, … would break the fixed-name contract; docker
/// compose refuses this too, so podup fails fast with the same guidance.
pub(super) fn check_fixed_name_scale(
	service_name: &str,
	service: &Service,
	replicas: usize,
) -> Result<()> {
	if replicas > 1 && service.container_name.is_some() {
		return Err(ComposeError::Unsupported(format!(
			"service '{service_name}' sets a fixed container_name but is scaled to {replicas} \
			 replicas; a fixed container_name can name only one container. Remove container_name \
			 to scale, or keep the service at a single replica."
		)));
	}
	Ok(())
}

/// The trailing `-<N>` numeric index of a replica container name, if any
/// (e.g. `proj-web-2` -> `Some(2)`).
fn trailing_index(name: &str) -> Option<usize> {
	name.rsplit_once('-')
		.and_then(|(_, suffix)| suffix.parse().ok())
}

/// Sort live replica names into the same deterministic ascending order the
/// static [`Engine::replica_names_for`] path always produces (`-1, -2, -3,
/// ...`). libpod's `/containers/json` does not sort its results, so without
/// this a scaled service's `logs`/by-service lifecycle output order would
/// depend on whatever order the daemon happens to return, drifting between
/// polls. A name without a parseable trailing index (an unusual custom
/// `container_name`) sorts after every indexed name, falling back to a
/// lexical compare so it never panics.
fn sort_replica_names(names: &mut [String]) {
	names.sort_by(|a, b| match (trailing_index(a), trailing_index(b)) {
		(Some(ia), Some(ib)) => ia.cmp(&ib),
		(Some(_), None) => std::cmp::Ordering::Less,
		(None, Some(_)) => std::cmp::Ordering::Greater,
		(None, None) => a.cmp(b),
	});
}

impl Engine {
	/// Set the number of running containers for the named services (docker
	/// `compose scale SERVICE=N`). Creates missing replicas and removes any
	/// surplus. The `--scale` overrides are already applied to this engine, so
	/// `resolve_replicas` reports the target count during the up pass.
	pub async fn scale(&self, file: &ComposeFile, pairs: &[(String, u32)]) -> Result<()> {
		for (svc, _) in pairs {
			if !file.services.contains_key(svc) {
				return Err(ComposeError::ServiceNotFound(svc.clone()));
			}
		}
		// Fail fast on an over-limit count, a fixed host port, or a fixed
		// container_name before touching any container.
		for (svc, target) in pairs {
			check_replica_limit(svc, *target as usize)?;
			check_scale_port_conflict(svc, &file.services[svc], *target as usize)?;
			check_fixed_name_scale(svc, &file.services[svc], *target as usize)?;
		}
		// Create the missing replicas and prune any surplus. Both halves run on
		// the shared `up` path, which reconciles every service carrying an active
		// `--scale` override against the last-wins target (so duplicate pairs such
		// as `svc=1 svc=3` can no longer drive create and prune to disagree).
		let targets: Vec<String> = pairs.iter().map(|(s, _)| s.clone()).collect();
		self.up_with_options(file, true, &[], &targets, true, false, true)
			.await?;
		Ok(())
	}

	/// Remove the containers of `service_name` whose names fall outside the
	/// desired `target`-replica set (the scale-down half of reconciliation).
	/// Surplus containers are stopped and removed concurrently so a large
	/// scale-down costs roughly one grace period rather than one per replica.
	///
	/// Best-effort across every surplus replica — one that fails to stop/remove
	/// must not block the rest from being reclaimed — but the first real
	/// failure is remembered and returned once every replica has been
	/// attempted, so `scale`/`up --scale` does not exit 0 with a surplus
	/// replica silently left running (#598).
	pub(super) async fn remove_surplus_replicas(
		&self,
		service_name: &str,
		service: &Service,
		target: u32,
	) -> Result<()> {
		// The desired set is the index-suffixed name at every count (`svc-1`
		// even for a single replica), so a scale N→1 keeps the running `svc-1`
		// instead of treating the bare `svc` as desired and destroying every
		// numbered replica (#815).
		let desired: HashSet<String> = self
			.replica_names_for(service_name, service, target as usize)
			.into_iter()
			.collect();
		let grace = self.grace_period_secs(service);
		let surplus: Vec<String> = self
			.list_project_container_names(Some(service_name))
			.await?
			.into_iter()
			.filter(|name| !desired.contains(name))
			.collect();
		// Scaling down removes surplus replicas but keeps their data volumes
		// (only `down -v` reclaims volumes).
		let results = futures_util::future::join_all(
			surplus
				.iter()
				.map(|name| self.stop_and_remove(name, grace, false)),
		)
		.await;
		first_error(results).map_or(Ok(()), Err)
	}

	/// Stop (best-effort) then force-remove a container by name. With
	/// `remove_volumes`, the container's anonymous volumes are reclaimed too
	/// (`podman rm -v`), so a label-based teardown sweep does not leave image
	/// `VOLUME`/anonymous volumes behind. "No such container" (404) is an
	/// idempotent no-op; any other removal failure propagates instead of being
	/// swallowed into a debug log.
	pub(super) async fn stop_and_remove(
		&self,
		name: &str,
		grace: i32,
		remove_volumes: bool,
	) -> Result<()> {
		// Bound the stop by the grace window: the force-remove below SIGKILLs the
		// container, so a stop that stalls past the grace must not pin us for the
		// full client READ_TIMEOUT before we fall through to it.
		let stop_path = format!(
			"{API_PREFIX}/containers/{}/stop?t={}",
			urlencoded(name),
			stop_timeout_param(grace),
		);
		let _ = self
			.client
			.post_empty_ok_within(&stop_path, stop_deadline(grace))
			.await;
		let rm_path = super::container_rm_path(name, remove_volumes);
		match self.client.delete_ok(&rm_path).await {
			Ok(()) => {
				crate::ui::progress_line("Container", name, "Removed");
				Ok(())
			}
			Err(e) if e.is_status(404) => {
				tracing::debug!("scale-down rm {name}: already gone ({e})");
				Ok(())
			}
			Err(e) => Err(ComposeError::Podman(e)),
		}
	}

	/// All container names carrying this project's label, optionally narrowed to
	/// one service via the `podup.service` label. Lets reconciliation find
	/// scaled replicas that the compose file's default count no longer names.
	pub(crate) async fn list_project_container_names(
		&self,
		service: Option<&str>,
	) -> Result<Vec<String>> {
		let mut labels = vec![format!("podup.project={}", self.project)];
		if let Some(svc) = service {
			labels.push(format!("podup.service={svc}"));
		}
		let filters = serde_json::json!({ "label": labels });
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			urlencoded(&filters.to_string()),
		);
		let entries = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;
		Ok(entries
			.into_iter()
			.flat_map(|e| e.names)
			.map(|raw| raw.trim_start_matches('/').to_string())
			.collect())
	}

	/// All live project containers grouped by their `podup.service` label, in a
	/// single API call. Lets a whole-project command (e.g. `down`) avoid one
	/// per-service container-list round-trip; callers fall back to the static
	/// [`Engine::replica_names`] for a service absent from the map.
	pub(crate) async fn list_project_containers_by_service(
		&self,
	) -> Result<std::collections::HashMap<String, Vec<String>>> {
		let filters = serde_json::json!({ "label": [format!("podup.project={}", self.project)] });
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			urlencoded(&filters.to_string()),
		);
		let entries = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;
		let mut by_service: std::collections::HashMap<String, Vec<String>> =
			std::collections::HashMap::new();
		for entry in entries {
			let Some(service) = entry.labels.get("podup.service") else {
				continue;
			};
			if let Some(raw) = entry.names.first() {
				by_service
					.entry(service.clone())
					.or_default()
					.push(raw.trim_start_matches('/').to_string());
			}
		}
		Ok(by_service)
	}

	/// The live containers of a service (matched by the `podup.service` label),
	/// each paired with its machine-readable state (`running`, `created`,
	/// `exited`, `paused`, …). Unlike [`Self::live_replica_names`] this does NOT
	/// fall back to statically-predicted names: a service with no live container
	/// yields an empty vec, so a lifecycle op (stop/wait/…) on a defined-but-
	/// never-created service is a quiet no-op instead of POSTing to a phantom
	/// name and surfacing a raw 404 (#758). The state lets `stop` report
	/// "stopped" only for containers that were actually running (#876).
	pub(crate) async fn live_service_containers(
		&self,
		service_name: &str,
	) -> Result<Vec<LiveContainer>> {
		let filters = serde_json::json!({
			"label": [
				format!("podup.project={}", self.project),
				format!("podup.service={service_name}"),
			],
		});
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			urlencoded(&filters.to_string()),
		);
		let entries = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;
		Ok(entries
			.into_iter()
			.filter_map(|e| {
				let state = e.state;
				e.names.into_iter().next().map(|raw| LiveContainer {
					name: raw.trim_start_matches('/').to_string(),
					state,
				})
			})
			.collect())
	}

	/// The container names to act on for a service: the ones Podman actually has
	/// (matched by the `podup.service` label), so lifecycle and query commands
	/// keep working after a runtime `scale`/`up --scale` that the compose file's
	/// static replica count no longer names. Falls back to the statically-derived
	/// names when none exist yet (e.g. a service not yet created). Live names are
	/// always returned in ascending `-1, -2, -3, ...` order (see
	/// [`sort_replica_names`]), matching the static path regardless of the order
	/// libpod's container list happens to report.
	pub(crate) async fn live_replica_names(
		&self,
		service_name: &str,
		service: &Service,
	) -> Result<Vec<String>> {
		let mut live = self
			.list_project_container_names(Some(service_name))
			.await?;
		Ok(if live.is_empty() {
			self.replica_names(service_name, service)
		} else {
			sort_replica_names(&mut live);
			live
		})
	}

	/// The names of a service's containers that are actually **running**, in the
	/// same ascending `-1, -2, -3, ...` order as [`Self::live_replica_names`].
	///
	/// For the query commands that only mean anything against a live process
	/// (`top`), where a stopped replica has to be skipped rather than asked: the
	/// libpod endpoints answer a non-running container with an HTTP 500, which
	/// callers must not have to tell apart from a real failure by parsing its
	/// message. Unlike [`Self::live_replica_names`] there is no fallback to
	/// statically-derived names — a service that was never created has nothing
	/// running, so it yields an empty vec instead of a phantom name.
	pub(crate) async fn running_replica_names(&self, service_name: &str) -> Result<Vec<String>> {
		let mut running: Vec<String> = self
			.live_service_containers(service_name)
			.await?
			.into_iter()
			.filter(|c| c.state.eq_ignore_ascii_case("running"))
			.map(|c| c.name)
			.collect();
		sort_replica_names(&mut running);
		Ok(running)
	}
}

#[cfg(test)]
mod tests {
	#[cfg(unix)]
	use crate::engine::fake_podman;
	#[cfg(unix)]
	use crate::engine::Engine;
	#[cfg(unix)]
	use crate::error::ComposeError;

	#[cfg(unix)]
	fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
		Engine::with_base_dir(client, project.into(), std::env::temp_dir())
	}

	/// #598: a `scale`/`up --scale` down-sizing that can't remove a surplus
	/// replica (e.g. an active exec session) must not exit 0 with it left
	/// running — but a sibling replica that removes cleanly must still be
	/// reclaimed.
	#[tokio::test]
	#[cfg(unix)]
	async fn remove_surplus_replicas_propagates_a_real_rm_failure_after_completing_the_rest() {
		let live = r#"[{"Names":["/proj-web-1"]},{"Names":["/proj-web-2"]}]"#;
		let fake = fake_podman::start(move |method, target| {
			if method == "GET" && target.contains("/containers/json") {
				(200, live.to_string())
			} else if (method == "POST" && target.contains("/stop"))
				|| (method == "DELETE" && target.contains("/proj-web-1?force=true"))
			{
				(200, String::new())
			} else if method == "DELETE" && target.contains("/proj-web-2?force=true") {
				(500, r#"{"message":"device or resource busy"}"#.to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = engine_with(fake.client(), "proj");

		// target = 0 desired replicas, so every live container is surplus
		// (mirrors the `replica_names_for_zero_scale_is_empty` contract).
		let err = e
			.remove_surplus_replicas("web", &crate::compose::types::Service::default(), 0)
			.await
			.expect_err("a real surplus-removal failure must propagate");
		assert!(
			matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
			"got {err:?}"
		);

		let seen = fake.requests.lock().unwrap();
		assert!(
			seen.iter()
				.any(|r| r.contains("DELETE") && r.contains("/proj-web-1?force=true")),
			"expected proj-web-1 to still be removed despite proj-web-2 failing: {seen:?}"
		);
	}

	/// Surplus replicas that are already gone (404 on removal) stay an
	/// idempotent no-op — a re-run of `scale` down must still exit 0.
	#[tokio::test]
	#[cfg(unix)]
	async fn remove_surplus_replicas_tolerates_already_gone() {
		let live = r#"[{"Names":["/proj-web-1"]}]"#;
		let fake = fake_podman::start(move |method, target| {
			if method == "GET" && target.contains("/containers/json") {
				(200, live.to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = engine_with(fake.client(), "proj");
		e.remove_surplus_replicas("web", &crate::compose::types::Service::default(), 0)
			.await
			.expect("an already-gone surplus replica must still exit 0");
	}

	/// libpod's `/containers/json` does not guarantee order; `logs` and every
	/// other by-service lifecycle/query command must still see a scaled
	/// service's replicas in the same ascending `-1, -2, -3` order the static
	/// `replica_names_for` path always produces, even when the fake (like a
	/// real libpod) hands them back shuffled.
	#[tokio::test]
	#[cfg(unix)]
	async fn live_replica_names_sorts_shuffled_replicas_ascending() {
		let containers = r#"[
			{"Names":["/proj-web-3"]},
			{"Names":["/proj-web-1"]},
			{"Names":["/proj-web-2"]}
		]"#;
		let fake = fake_podman::start(move |method, target| {
			if method == "GET" && target.contains("/containers/json") {
				(200, containers.to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = engine_with(fake.client(), "proj");

		let names = e
			.live_replica_names("web", &crate::compose::types::Service::default())
			.await
			.expect("live_replica_names should succeed");

		assert_eq!(
			names,
			vec![
				"proj-web-1".to_string(),
				"proj-web-2".to_string(),
				"proj-web-3".to_string(),
			]
		);
	}

	/// #1250: `top` aborted on a project with a stopped service because it asked
	/// every container that exists for its process list, and libpod answers a
	/// non-running one with an HTTP 500. The exited replica must be dropped
	/// before the call, and the survivors must keep the ascending order every
	/// other by-service command produces — so this asserts both, on a listing
	/// that is shuffled and mixed-state at once.
	#[tokio::test]
	#[cfg(unix)]
	async fn running_replica_names_drops_non_running_and_keeps_ascending_order() {
		let containers = r#"[
			{"Names":["/proj-web-3"],"State":"running"},
			{"Names":["/proj-web-4"],"State":"created"},
			{"Names":["/proj-web-1"],"State":"running"},
			{"Names":["/proj-web-5"],"State":"paused"},
			{"Names":["/proj-web-2"],"State":"exited"}
		]"#;
		let fake = fake_podman::start(move |method, target| {
			if method == "GET" && target.contains("/containers/json") {
				(200, containers.to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = engine_with(fake.client(), "proj");

		let names = e
			.running_replica_names("web")
			.await
			.expect("running_replica_names should succeed");

		assert_eq!(
			names,
			vec!["proj-web-1".to_string(), "proj-web-3".to_string()],
			"only the running replicas, in ascending order"
		);
	}

	/// The sibling half of the rule above: a service that exists in the compose
	/// file but was never created has nothing running, so `top` must render
	/// nothing for it rather than fall back to a statically-derived name and
	/// then have to swallow the 404 that name earns.
	#[tokio::test]
	#[cfg(unix)]
	async fn running_replica_names_does_not_fall_back_to_static_names() {
		let fake = fake_podman::start(|method, target| {
			if method == "GET" && target.contains("/containers/json") {
				(200, "[]".to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = engine_with(fake.client(), "proj");

		let names = e
			.running_replica_names("web")
			.await
			.expect("running_replica_names should succeed");

		assert!(
			names.is_empty(),
			"a never-created service yields no names, got {names:?}"
		);
	}

	use super::{
		check_fixed_name_scale, check_replica_limit, check_scale_port_conflict, state_is_active,
		DEFAULT_MAX_REPLICAS,
	};

	#[test]
	fn state_is_active_only_for_running_and_paused() {
		// `stop` actually transitions only a running or paused container; for any
		// other state it is a no-op that must not be reported as "stopped" (#876).
		assert!(state_is_active("running"));
		assert!(state_is_active("paused"));
		assert!(!state_is_active("created"));
		assert!(!state_is_active("exited"));
		assert!(!state_is_active("stopped"));
		assert!(!state_is_active("dead"));
		assert!(!state_is_active("configured"));
		assert!(!state_is_active(""));
	}

	#[test]
	fn replica_limit_default_and_env_override() {
		// One test owns the shared `PODUP_MAX_REPLICAS` env var for its whole body
		// so a sibling test running in parallel can never race it.
		let max = DEFAULT_MAX_REPLICAS as usize;

		// Default ceiling: at-limit allowed, over-limit rejected.
		std::env::remove_var("PODUP_MAX_REPLICAS");
		assert!(check_replica_limit("web", 1).is_ok());
		assert!(check_replica_limit("web", max).is_ok());
		let err = check_replica_limit("web", max + 1).unwrap_err();
		assert!(matches!(
			err,
			crate::error::ComposeError::ReplicaLimitExceeded { .. }
		));
		assert!(check_replica_limit("web", 100_000).is_err());

		// Env override lowers the ceiling.
		std::env::set_var("PODUP_MAX_REPLICAS", "2");
		assert!(check_replica_limit("web", 2).is_ok());
		assert!(check_replica_limit("web", 3).is_err());

		// A zero/garbage override falls back to the default ceiling.
		std::env::set_var("PODUP_MAX_REPLICAS", "0");
		assert!(check_replica_limit("web", max).is_ok());
		std::env::set_var("PODUP_MAX_REPLICAS", "nope");
		assert!(check_replica_limit("web", max).is_ok());
		std::env::remove_var("PODUP_MAX_REPLICAS");
	}

	fn service(yaml: &str) -> crate::compose::types::Service {
		let file = crate::parse_str(yaml).unwrap();
		file.services.into_iter().next().unwrap().1
	}

	#[test]
	fn single_replica_never_conflicts() {
		let svc = service("services:\n  web:\n    image: x\n    ports:\n      - \"8080:80\"\n");
		assert!(check_scale_port_conflict("web", &svc, 1).is_ok());
	}

	#[test]
	fn scaled_fixed_host_port_conflicts() {
		let svc = service("services:\n  web:\n    image: x\n    ports:\n      - \"8080:80\"\n");
		let err = check_scale_port_conflict("web", &svc, 3).unwrap_err();
		assert!(matches!(
			err,
			crate::error::ComposeError::ScalePortConflict { .. }
		));
		assert!(err.to_string().contains("8080"));
	}

	#[test]
	fn scaled_random_host_port_is_allowed() {
		// A container-only port (`"80"`) gets a runtime-assigned host port per
		// replica, so scaling is fine.
		let svc = service("services:\n  web:\n    image: x\n    ports:\n      - \"80\"\n");
		assert!(check_scale_port_conflict("web", &svc, 3).is_ok());
	}

	#[test]
	fn scaled_no_ports_is_allowed() {
		let svc = service("services:\n  worker:\n    image: x\n");
		assert!(check_scale_port_conflict("worker", &svc, 5).is_ok());
	}

	#[test]
	fn fixed_container_name_single_replica_is_allowed() {
		let svc = service("services:\n  app:\n    image: x\n    container_name: myapp\n");
		assert!(check_fixed_name_scale("app", &svc, 1).is_ok());
	}

	#[test]
	fn fixed_container_name_scaled_above_one_is_rejected() {
		let svc = service("services:\n  app:\n    image: x\n    container_name: myapp\n");
		let err = check_fixed_name_scale("app", &svc, 3).unwrap_err();
		assert!(matches!(err, crate::error::ComposeError::Unsupported(_)));
		assert!(err.to_string().contains("container_name"));
	}

	#[test]
	fn unnamed_service_scales_freely() {
		let svc = service("services:\n  app:\n    image: x\n");
		assert!(check_fixed_name_scale("app", &svc, 5).is_ok());
	}
}
