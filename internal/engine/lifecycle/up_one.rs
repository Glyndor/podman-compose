//! Per-service `up` orchestration: wait on `depends_on`, build/pull the
//! image, create/start each replica. Split out of [`super::up`] so the
//! dependency-level walker stays a short sequence and the per-service
//! helpers stay close to their dependencies (the readiness map, the
//! scale guards, the level parallel runner).

use std::collections::{HashMap, HashSet};

use crate::compose::types::{ComposeFile, ServiceCondition};
use crate::error::Result;

use super::readiness::SharedReady;

use super::super::container::config_hash;
use super::super::Engine;

impl Engine {
	/// Bring up a single service: honor profile/target filters, wait on its
	/// `depends_on` conditions, build or pull the image, and create/start each
	/// replica (skipping containers that are unchanged unless `force_recreate`).
	/// Used by [`super::run_up`]; safe to run concurrently for services in
	/// the same dependency level (the `Engine` holds no per-call mutable
	/// state — the libpod client is connection-per-request).
	#[allow(clippy::too_many_arguments)]
	pub(crate) async fn up_one_service(
		&self,
		name: &str,
		file: &ComposeFile,
		enabled: &HashSet<String>,
		target_set: &Option<HashSet<String>>,
		existing: &HashMap<String, super::ExistingContainer>,
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
			if !super::targets::in_started_set(target_set, &dep) {
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
								.map_err(|e| super::readiness::unshare_readiness_error(&e)),
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
		super::scale::check_replica_limit(name, replicas)?;
		// A scaled service that publishes a fixed host port cannot start: only
		// one container can bind it. Fail fast with guidance instead of letting
		// replicas 2..N die mid-up with `address already in use`.
		super::scale::check_scale_port_conflict(name, service, replicas)?;
		// A service pinning an explicit container_name cannot be scaled past one
		// replica without violating its fixed-name contract; reject it rather
		// than inventing `name-1`, `name-2`, … (docker compose refuses this too).
		super::scale::check_fixed_name_scale(name, service, replicas)?;

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
					existing,
					&new_hash,
					no_recreate,
					force_recreate,
					start,
				)
			});
		if let Some(e) = super::parallel::first_error(super::parallel::join_bounded(futs).await) {
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
	/// `super::scale::check_scale_port_conflict`.
	#[allow(clippy::too_many_arguments)]
	pub(crate) async fn up_one_replica(
		&self,
		container_name: String,
		name: &str,
		service: &crate::compose::types::Service,
		file: &ComposeFile,
		existing: &HashMap<String, super::ExistingContainer>,
		new_hash: &str,
		no_recreate: bool,
		force_recreate: bool,
		start: bool,
	) -> Result<()> {
		let present = existing.contains_key(&container_name);
		if !force_recreate {
			if no_recreate && present {
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
			if self
				.unchanged(&container_name, name, service, existing, new_hash)
				.await?
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
		let (doing, done) = match (present, start) {
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
