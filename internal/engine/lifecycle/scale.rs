//! The `scale` subcommand plus the replica-listing/reconciliation helpers
//! shared with teardown.

use std::collections::HashSet;

use crate::compose::types::{ComposeFile, Service};
use crate::engine::Engine;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::parallel::first_error;
use super::targets::{stop_deadline, stop_timeout_param};

/// Whether a container in this state is currently active, i.e. `stop` would
/// actually transition it. A `running` or `paused` container is stopped; a
/// `created`/`exited`/`dead`/… one is already not running, so stopping it is a
/// no-op that must not be reported as "stopped" (#876). Pure for unit testing.
#[allow(dead_code)]
pub(crate) fn state_is_active(state: &str) -> bool {
	matches!(state, "running" | "paused")
}

/// The default ceiling on a service's replica count.
pub(super) const DEFAULT_MAX_REPLICAS: u32 = 256;

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
		self.up_with_options(file, true, &[], &targets, true, false, true, false)
			.await?;
		Ok(())
	}

	/// Remove the containers of `service_name` whose names fall outside the
	/// desired `target`-replica set (the scale-down half of reconciliation).
	/// Surplus containers are stopped and removed concurrently so a large
	/// scale-down costs roughly one grace period rather than one per replica.
	///
	/// Best-effort across every surplus replica (one that fails to stop/remove
	/// must not block the rest from being reclaimed) but the first real
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
		// Open the row before the stop, not after: with the default grace the
		// stop can take ten seconds, and a row that appears only at `Removed`
		// has no start time and says nothing while the container winds down
		// (#1686).
		crate::ui::progress::start("Container", name, "Stopping");
		let stop_path = format!(
			"{API_PREFIX}/containers/{}/stop?t={}",
			urlencoded(name),
			stop_timeout_param(grace),
		);
		let _ = self
			.client
			.post_empty_ok_within(&stop_path, stop_deadline(grace))
			.await;
		crate::ui::progress::start("Container", name, "Removing");
		let rm_path = super::container_rm_path(name, remove_volumes);
		match self.client.delete_ok(&rm_path).await {
			Ok(()) => {
				crate::ui::progress_line("Container", name, "Removed");
				Ok(())
			}
			Err(e) if e.is_status(404) => {
				tracing::debug!("scale-down rm {name}: already gone ({e})");
				crate::ui::progress_line("Container", name, "Absent");
				Ok(())
			}
			Err(e) => {
				crate::ui::progress_line("Container", name, "Failed");
				Err(ComposeError::Podman(e))
			}
		}
	}

	/// All container names carrying this project's label, optionally narrowed to
	/// one service via the `podup.service` label. Lets reconciliation find
	/// scaled replicas that the compose file's default count no longer names.
	pub(crate) async fn list_project_container_names(
		&self,
		service: Option<&str>,
	) -> Result<Vec<String>> {
		// The project label half comes from the cached `project_label_raw`;
		// the optional service label is appended fresh (#1364).
		let mut labels = vec![self.project_label_raw().to_string()];
		if let Some(svc) = service {
			labels.push(format!("podup.service={svc}"));
		}
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			self.project_label_filter_with(labels.iter().cloned()),
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

	/// All project containers grouped by their `podup.service` label, in a single
	/// API call. Lets a whole-project command (`stop`/`start`/`restart`/`kill`/
	/// `rm`/`pause`/`unpause`/`down`) avoid one per-service container-list
	/// round-trip (#1363): every per-service lifecycle command prefetches the
	/// project's containers once via this helper and then reads its per-service
	/// slice from the in-memory map, collapsing S+1 GETs into 1.
	///
	/// `all=true` is set explicitly: libpod's container-list defaults to a
	/// `runningOnly` filter when no `status` filter is supplied, so a bare
	/// project GET would silently drop the very containers `start`/`restart`/
	/// `kill`/`rm`/`pause`/`unpause` need to act on (#1363 validation).
	/// Callers fall back to the static [`Engine::replica_names`] for a service
	/// absent from the map.
	pub(crate) async fn live_project_replicas(
		&self,
	) -> Result<std::collections::HashMap<String, Vec<String>>> {
		// Reuse the per-engine URL-encoded filter (#1364); see
		// [`Engine::project_label_filter_encoded`].
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			self.project_label_filter_encoded(),
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

	/// The bulk project listing ([`Self::live_project_replicas`]) with each
	/// per-service bucket sorted into the same ascending `-1, -2, -3, ...`
	/// order the static [`Engine::replica_names`] path produces. Lets the
	/// per-replica query paths (`exec`/`cp` via `live_replica_name_at`,
	/// `port` via `port_with_index`, `logs`) read each service's names off a
	/// single shared map instead of issuing one container-list round-trip per
	/// service (#1445): a `podup logs` over a 40-service project now costs 1
	/// GET, not 40, with the same ordering the per-service helper it replaced
	/// was pinned to.
	///
	/// Same `all=true` contract as [`Self::live_project_replicas`]: stopped
	/// replicas are kept on purpose so a service scaled beyond its static
	/// compose count (or never reaped by a prior `down`) is not silently
	/// dropped from the bucket. The static-name fallback for a service absent
	/// from the map is the *caller's* responsibility; this helper returns an
	/// empty vec for one, since it does not see the compose file.
	pub(crate) async fn live_project_replicas_sorted(
		&self,
	) -> Result<std::collections::HashMap<String, Vec<String>>> {
		let mut by_service = self.live_project_replicas().await?;
		for names in by_service.values_mut() {
			sort_replica_names(names);
		}
		Ok(by_service)
	}

	/// Sibling of [`Self::live_project_replicas_sorted`] that filters each
	/// bucket to running containers only. Built for `podup top`, where the
	/// libpod `/top` endpoint answers a non-running container with an HTTP 500
	/// and the caller would otherwise have to skip it after the fact (#1250).
	/// One bulk GET powers the whole-project `top` the way
	/// `live_project_replicas_sorted` already powers `logs`/`port`/`exec`/`cp`
	/// (#1445): a 40-service `top` used to issue 40 `/containers/json` GETs,
	/// one per service, and now issues 1 (#1742).
	///
	/// Same per-service sort as the sorted sibling, so the printed order is
	/// the same ascending `-1, -2, -3` order `top` used to produce from the
	/// per-service helper. The static-name fallback is the caller's
	/// responsibility: a service absent from the map yields no names here.
	pub(crate) async fn live_project_running_replicas_sorted(
		&self,
	) -> Result<std::collections::HashMap<String, Vec<String>>> {
		// Reuse the per-engine URL-encoded filter (#1364); see
		// [`Engine::project_label_filter_encoded`].
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			self.project_label_filter_encoded(),
		);
		let entries = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;
		let mut by_service: std::collections::HashMap<String, Vec<String>> =
			std::collections::HashMap::new();
		for entry in entries {
			// The `/top` endpoint answers a non-running container with an
			// HTTP 500; dropping the stopped replicas here is what kept the
			// per-service `running_replica_names` helper silent on a mixed
			// listing (#1250).
			if !entry.state.eq_ignore_ascii_case("running") {
				continue;
			}
			let Some(service) = entry.labels.get("podup.service") else {
				continue;
			};
			let Some(raw) = entry.names.first() else {
				continue;
			};
			by_service
				.entry(service.clone())
				.or_default()
				.push(raw.trim_start_matches('/').to_string());
		}
		for names in by_service.values_mut() {
			sort_replica_names(names);
		}
		Ok(by_service)
	}
}
