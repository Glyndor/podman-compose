//! Service lifecycle commands: up, down, start, stop, restart, kill, rm, pause, unpause, run.

mod commands;
mod down_label;
mod drop_recheck;
mod images;
// Visible within the engine, not beyond it: the secret pre-creation stage
// (#1219) fans out against the same `MAX_LIFECYCLE_CONCURRENCY` ceiling rather
// than growing a second concurrency limit that could silently drift from this
// one.
mod options;
pub(in crate::engine) mod parallel;
mod prefetch;
mod readiness;
mod run;
mod run_attached;
mod scale;
mod schedule;
mod seed;
mod signal;
mod targets;
mod teardown;

use std::collections::{HashMap, HashSet};

use crate::compose::types::{ComposeFile, Service, ServiceCondition};
use crate::error::Result;
use crate::libpod::API_PREFIX;

use readiness::SharedReady;

pub use options::{RunOptions, RunOverrides};
pub use targets::validate_stop_timeout;
use targets::{expand_targets, filter_services, in_started_set, validate_targets};

use super::container::config_hash;

use super::profiles::{active_profiles_set, enabled_profile_services};
use super::Engine;

impl Engine {
	/// Start all services defined in the compose file, creating containers that do not exist.
	pub async fn up(&self, file: &ComposeFile) -> Result<()> {
		self.up_with_options(file, false, &[], &[], false, false, false)
			.await
	}

	/// Start a container by name. Used when `up` leaves an unchanged container in
	/// place but wants to ensure it is running. "Already in the desired state"
	/// (304) and "no such container" (404) are idempotent no-ops, matching
	/// [`Self::run_lifecycle_op`]; any other failure (e.g. the container's
	/// published port is now taken) propagates instead of being swallowed.
	async fn ensure_started(&self, container_name: &str) -> Result<()> {
		let path = format!(
			"{API_PREFIX}/containers/{}/start",
			crate::libpod::urlencoded(container_name)
		);
		match self.client.post_empty_ok(&path).await {
			Ok(()) => Ok(()),
			Err(e) if e.is_status(404) => {
				tracing::debug!("{container_name}: start skipped ({e})");
				Ok(())
			}
			Err(e) => Err(crate::error::ComposeError::Podman(e)),
		}
	}

	/// Start services with explicit options. When `no_recreate` is true, running containers are left untouched. On partial failure, staging directories are cleaned up.
	#[allow(clippy::too_many_arguments)]
	pub async fn up_with_options(
		&self,
		file: &ComposeFile,
		_detach: bool,
		active_profiles: &[String],
		target_services: &[String],
		no_recreate: bool,
		force_recreate: bool,
		no_deps: bool,
	) -> Result<()> {
		self.run_up(
			file,
			active_profiles,
			target_services,
			no_recreate,
			force_recreate,
			no_deps,
			true,
		)
		.await
	}

	/// Create containers for services without starting them (docker compose
	/// `create`). Shares the `up` path with `start = false`: images are built/
	/// pulled and containers created, but never started, and no `depends_on`
	/// waits or `post_start` hooks run (nothing is running to gate on).
	#[allow(clippy::too_many_arguments)]
	pub async fn create_with_options(
		&self,
		file: &ComposeFile,
		active_profiles: &[String],
		target_services: &[String],
		no_recreate: bool,
		force_recreate: bool,
		no_deps: bool,
	) -> Result<()> {
		self.run_up(
			file,
			active_profiles,
			target_services,
			no_recreate,
			force_recreate,
			no_deps,
			false,
		)
		.await
	}

	#[allow(clippy::too_many_arguments)]
	async fn run_up(
		&self,
		file: &ComposeFile,
		active_profiles: &[String],
		target_services: &[String],
		no_recreate: bool,
		force_recreate: bool,
		no_deps: bool,
		start: bool,
	) -> Result<()> {
		let result = async {
			// Reject any volume/network/container name Podman's regex would refuse
			// before issuing a single create, so a bad name surfaces as a clear
			// client-side error (not an opaque HTTP 500) with nothing created.
			self.validate_object_names(file)?;

			let levels = crate::compose::resolve_levels(file)?;
			let active = active_profiles_set(active_profiles);
			// Which services this `up`/`create` should start. A profiled service
			// that an active service depends on is implicitly activated here —
			// the same set `config` reports — so `up` never leaves a started
			// service with an unsatisfied (never-created) dependency.
			let enabled = enabled_profile_services(file, &active, target_services);

			// Validate every `--scale SERVICE=N` override against the file before
			// doing any work: an override naming a service the compose file does
			// not define is a user error, not a silent no-op (the standalone
			// `scale` subcommand already rejects it, so the `up` path must too).
			for svc in self.scale_overrides.keys() {
				if !file.services.contains_key(svc) {
					return Err(crate::error::ComposeError::ServiceNotFound(svc.clone()));
				}
			}

			// Reject unknown service names before doing any work, so `up`/`create`
			// of a bogus service errors instead of exiting 0 as a silent no-op.
			validate_targets(file, target_services)?;
			let target_set = expand_targets(file, target_services, no_deps);

			// Prefetch the project's containers once (instead of one API call per
			// replica): which names already exist, and each one's config-hash
			// label so we can decide whether a container needs recreation.
			//
			// Fetched on the `--force-recreate` path too. The hash is unused
			// there, but `present` is what tells the progress stream that a
			// container was replaced rather than created (#1619), and a forced
			// recreate is exactly the case where that word matters.
			let mut present: HashSet<String> = HashSet::new();
			let mut existing_hash: HashMap<String, String> = HashMap::new();
			{
				let path = format!(
					"{API_PREFIX}/containers/json?all=true&filters={}",
					self.project_label_filter_encoded(),
				);
				let entries = self
					.client
					.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
					.await
					.map_err(crate::error::ComposeError::Podman)?;
				for entry in entries {
					if let Some(hash) = entry.labels.get("podup.config-hash") {
						for raw in &entry.names {
							existing_hash
								.insert(raw.trim_start_matches('/').to_string(), hash.clone());
						}
					}
					for raw in entry.names {
						present.insert(raw.trim_start_matches('/').to_string());
					}
				}
			}

			// Seed the board before any work starts. This is the whole point of
			// the phase: the resource set is knowable here — `levels` is already
			// resolved above, and the networks and volumes come straight off the
			// compose file — while every progress event in the tree fires once
			// its work is already over. A board grown from those events would be
			// a transcript with extra steps.
			crate::ui::progress::begin(self.up_resources(file, &enabled, &target_set));

			self.create_networks(file).await?;
			self.create_volumes(file).await?;
			// Pre-create the union of inline secrets/configs once, before the
			// concurrent per-level start loop, so two services in the same level
			// can't race the non-atomic delete-then-create of a shared name.
			self.create_project_secrets(file).await?;

			// Best-effort: warm the image cache for every service this pass will
			// pull, concurrently, before the per-level walk below serializes a
			// level-2+ service's image acquisition behind the level-1 barrier.
			// A prefetch I/O miss is never fatal — `up_one_service`'s own pull
			// below is still authoritative and the only path that can fail `up`
			// on a transport / registry error. A configuration error (an
			// unrecognized `pull_policy:`) does propagate: it is reported here
			// so the operator sees it, not a silent wrong image (#1443).
			self.prefetch_images(file, &enabled, &target_set).await?;

			// Start each dependency level in turn; services within a level have
			// no `depends_on` relationship to each other (guaranteed by the
			// layering), so they start concurrently. The barrier between levels
			// preserves ordering and `service_healthy`/`service_completed`
			// semantics: a level only begins once the previous one is up.
			// One shared healthcheck poller per waited-on container, so several
			// dependents in a level don't each run the same container's healthcheck.
			let readiness = self.build_readiness_map(file, &enabled, &target_set, start);

			self.start_services_by_dependency(
				&levels,
				file,
				&enabled,
				&target_set,
				&present,
				&existing_hash,
				no_recreate,
				force_recreate,
				start,
				&readiness,
			)
			.await?;

			// Reconcile surplus replicas for every service carrying an active
			// `--scale` override. Replica naming is unsuffixed for one replica and
			// suffixed (`svc-N`) for many, so scaling a service *down* on the `up`
			// path would otherwise leave the old higher-numbered containers running
			// (e.g. `up --scale web=3` then `up --scale web=1`). The overrides are a
			// last-wins map, so create (above) and this prune always agree on one
			// target count. Keyed off live container names inside
			// `remove_surplus_replicas`, this is the same reconciliation the `scale`
			// subcommand relies on.
			for (svc, &target) in &self.scale_overrides {
				let Some(service) = file.services.get(svc) else {
					continue;
				};
				if let Some(set) = &target_set {
					if !set.contains(svc) {
						continue;
					}
				}
				self.remove_surplus_replicas(svc, service, target).await?;
			}

			Ok(())
		}
		.await;

		// Close the board on every exit, not just the happy one: the region
		// hides the cursor, and a `?` that returned early through the block
		// above would otherwise leave the terminal without a caret. `end` is
		// idempotent and a no-op when no board was opened.
		crate::ui::progress::end();
		result
	}

	/// Bring up a single service: honor profile/target filters, wait on its
	/// `depends_on` conditions, build or pull the image, and create/start each
	/// replica (skipping containers that are unchanged unless `force_recreate`).
	/// Used by [`Self::up_with_options`]; safe to run concurrently for services
	/// in the same dependency level (the `Engine` holds no per-call mutable
	/// state — the libpod client is connection-per-request).
	#[allow(clippy::too_many_arguments)]
	async fn up_one_service(
		&self,
		name: &str,
		file: &ComposeFile,
		enabled: &HashSet<String>,
		target_set: &Option<HashSet<String>>,
		present: &HashSet<String>,
		existing_hash: &HashMap<String, String>,
		no_recreate: bool,
		force_recreate: bool,
		start: bool,
		readiness: &HashMap<String, SharedReady<'_>>,
	) -> Result<()> {
		if let Some(set) = target_set {
			if !set.contains(name) {
				return Ok(());
			}
		}
		let service = &file.services[name];

		if !enabled.contains(name) {
			tracing::debug!("skipping {name}: no active profile match");
			return Ok(());
		}

		// `create` (start = false) only builds the containers, so there is nothing
		// to gate on — skip the `depends_on` readiness waits entirely.
		for dep in service
			.depends_on
			.service_names()
			.into_iter()
			.filter(|_| start)
		{
			// Under `--no-deps` (and partial target lists) a dependency may have
			// been intentionally excluded from the started set. docker-compose
			// skips its readiness condition in that case; matching that avoids
			// waiting on (and 404-ing against) a container that was never
			// created.
			if !in_started_set(target_set, &dep) {
				tracing::debug!("{dep} not in started target set — skipping {name} readiness wait");
				continue;
			}

			let condition = service.depends_on.condition_for(&dep);
			// `required: false` makes the dependency optional — a failed wait
			// must not abort `up`, matching docker-compose v2.
			let required = service.depends_on.required_for(&dep);
			let dep_service = match file.services.get(&dep) {
				Some(s) => s,
				None => continue,
			};
			if !enabled.contains(&dep) {
				continue;
			}
			// Scaled dep has no base-named container; wait on its first replica.
			let dep_container = self.first_replica_name(&dep, dep_service);

			let wait = match condition {
				ServiceCondition::ServiceStarted => Ok(()),
				ServiceCondition::ServiceHealthy => {
					// Wait unless the healthcheck is explicitly disabled in
					// compose. With no compose healthcheck we still wait:
					// `wait_healthy` consults the container's effective
					// healthcheck, so image-inherited ones are honored and
					// the wait short-circuits when none exists.
					let disabled = dep_service
						.healthcheck
						.as_ref()
						.is_some_and(|h| h.is_disabled());
					if disabled {
						tracing::debug!(
							"{dep} healthcheck disabled — skipping service_healthy wait"
						);
						Ok(())
					} else {
						// Await the one shared poller for this container instead of
						// starting our own, so a container N services wait on has its
						// healthcheck run once per interval, not N times. Fall back to
						// a direct wait if the map somehow lacks this container.
						match readiness.get(&dep_container) {
							Some(shared) => shared
								.clone()
								.await
								.map_err(|e| readiness::unshare_readiness_error(&e)),
							None => self.wait_healthy(&dep_container, dep_service, None).await,
						}
					}
				}
				ServiceCondition::ServiceCompletedSuccessfully => {
					self.wait_completed(&dep_container).await
				}
			};
			match wait {
				Ok(()) => {}
				Err(e) if !required => {
					tracing::debug!(
						"optional dependency {dep} (required: false) did not satisfy its condition: {e}"
					);
				}
				Err(e) => return Err(e),
			}
		}

		self.acquire_service_image(name, service, file).await?;

		let replicas = self.resolve_replicas(name, service);
		// Bound the replica count (covers an untrusted compose `deploy.replicas`/
		// `scale:` as well as `--scale`) before creating any container.
		scale::check_replica_limit(name, replicas)?;
		// A scaled service that publishes a fixed host port cannot start: only
		// one container can bind it. Fail fast with guidance instead of letting
		// replicas 2..N die mid-up with `address already in use`.
		scale::check_scale_port_conflict(name, service, replicas)?;
		// A service pinning an explicit container_name cannot be scaled past one
		// replica without violating its fixed-name contract; reject it rather
		// than inventing `name-1`, `name-2`, … (docker compose refuses this too).
		scale::check_fixed_name_scale(name, service, replicas)?;

		let new_hash = config_hash(service, file)?;

		// Fan the replicas out with the same bounded concurrency the level
		// walk uses, instead of creating and starting them one at a time —
		// `up --scale web=5` used to pay 5x (create+start) in strict
		// sequence. Every replica is still attempted even when one fails
		// (`join_bounded` runs the whole batch), and `first_error` picks the
		// earliest one in replica-index order, so the reported failure stays
		// deterministic regardless of which replica's future happens to
		// finish first.
		let futs = self
			.replica_names_for(name, service, replicas)
			.into_iter()
			.map(|container_name| {
				self.up_one_replica(
					container_name,
					name,
					service,
					file,
					present,
					existing_hash,
					&new_hash,
					no_recreate,
					force_recreate,
					start,
				)
			});
		if let Some(e) = parallel::first_error(parallel::join_bounded(futs).await) {
			return Err(e);
		}

		Ok(())
	}

	/// Bring up one replica container of `service`: honor the `no_recreate`/
	/// config-hash skip logic, then fall through to create+start. One future
	/// in the per-service replica fan-out ([`Self::up_one_service`]); safe to
	/// run concurrently with the service's other replicas, since replicas of
	/// one service share no per-replica mutable state — a fixed host port
	/// that would make concurrent starts race is already rejected up front by
	/// [`scale::check_scale_port_conflict`].
	#[allow(clippy::too_many_arguments)]
	async fn up_one_replica(
		&self,
		container_name: String,
		name: &str,
		service: &Service,
		file: &ComposeFile,
		present: &HashSet<String>,
		existing_hash: &HashMap<String, String>,
		new_hash: &str,
		no_recreate: bool,
		force_recreate: bool,
		start: bool,
	) -> Result<()> {
		if !force_recreate {
			if no_recreate && present.contains(&container_name) {
				tracing::debug!("{container_name} already exists — skipping recreate");
				// `create` leaves an existing container as-is; `up` ensures it runs.
				if start {
					self.ensure_started(&container_name).await?;
				}
				crate::ui::progress_line(
					"Container",
					&container_name,
					if start { "Running" } else { "Exists" },
				);
				return Ok(());
			}
			// A service with a `build:` section is recreated even when its hash
			// matches: the compose-config hash compared below does not cover
			// the build context's source files, so the existing container may
			// still hold an image built from an older tree. Recreating forces
			// the new container to bind whatever image is current.
			//
			// `up` stopped rebuilding these unconditionally in #1094 — it now
			// builds only when the image is missing — so the fresh-image
			// guarantee this recreate used to inherit from that rebuild no
			// longer comes for free.
			if service.build.is_none()
				&& existing_hash.get(&container_name).map(String::as_str) == Some(new_hash)
			{
				tracing::debug!("{container_name} is up to date — skipping recreate");
				if start {
					self.ensure_started(&container_name).await?;
				}
				crate::ui::progress_line(
					"Container",
					&container_name,
					if start { "Running" } else { "Exists" },
				);
				return Ok(());
			}
		}
		// Replacing a container destroys its writable layer, and creating one
		// does not, so the two get different words. Before #1619 both printed
		// `Starting`/`Started`, and the only way to learn that a container had
		// been removed was to compare IDs by hand; `--force-recreate` and
		// `--no-recreate` were unverifiable from the output for the same reason.
		// `Recreating`/`Recreated` is docker compose's word for it too
		// (`Recreate`/`Recreated`, measured on v5.3.1), so a reader of either
		// tool's log reads the same event the same way.
		let existed = present.contains(&container_name);
		let (doing, done) = match (existed, start) {
			(true, _) => ("Recreating", "Recreated"),
			(false, true) => ("Starting", "Started"),
			(false, false) => ("Creating", "Created"),
		};
		crate::ui::progress::start("Container", &container_name, doing);
		self.create_and_start(&container_name, name, service, file, start)
			.await?;
		crate::ui::progress_line("Container", &container_name, done);

		// `post_start` hooks run inside a running container, so only on `up`.
		if start {
			for hook in &service.post_start {
				self.run_lifecycle_hook(&container_name, hook).await?;
			}
		}

		Ok(())
	}
}

/// Build the libpod container-removal path. `force` always terminates a running
/// container; with `remove_volumes` it also reclaims the anonymous volumes the
/// container owns (`podman rm -v` / `docker compose down -v` semantics). That is
/// the only way image `VOLUME` directives and short-form anonymous volumes get
/// removed: podup never names or labels them, so they cannot be enumerated and
/// deleted the way declared top-level volumes are.
pub(super) fn container_rm_path(name: &str, remove_volumes: bool) -> String {
	let with_volumes = if remove_volumes { "&v=true" } else { "" };
	format!(
		"{API_PREFIX}/containers/{}?force=true{with_volumes}",
		crate::libpod::urlencoded(name),
	)
}

#[cfg(test)]
mod drop_recheck_tests;
#[cfg(test)]
mod scale_tests;
#[cfg(test)]
mod teardown_tests;
#[cfg(test)]
mod tests;
