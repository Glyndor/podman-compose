//! `up` orchestration: validate, build the resource set, walk the dependency
//! levels. Split out of [`super::mod`] so the orchestrator stays under the
//! 400-line ceiling while the lifecycle's other commands (`down`, `run`,
//! `start`, …) stay close to their dedicated helpers.

use std::collections::HashMap;

use crate::compose::types::ComposeFile;
use crate::error::Result;

use crate::libpod::API_PREFIX;
use crate::ui::progress::Kind;

use super::super::profiles::{active_profiles_set, enabled_profile_services};
use super::super::Engine;

impl Engine {
	/// Start services with explicit options. When `no_recreate` is true, running containers are left untouched. On partial failure, staging directories are cleaned up.
	///
	/// `build` is the CLI `--build` flag: when set, every service with a
	/// `build:` block is built before the network/volume/container stages run,
	/// inside the same `up` board (`#1700`). The image rows are seeded at the
	/// top so the build verb appears above the container rows that depend on
	/// it.
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
		build: bool,
	) -> Result<()> {
		self.run_up(
			file,
			active_profiles,
			target_services,
			no_recreate,
			force_recreate,
			no_deps,
			true,
			build,
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
			false,
		)
		.await
	}

	#[allow(clippy::too_many_arguments)]
	pub(crate) async fn run_up(
		&self,
		file: &ComposeFile,
		active_profiles: &[String],
		target_services: &[String],
		no_recreate: bool,
		force_recreate: bool,
		no_deps: bool,
		start: bool,
		build: bool,
	) -> Result<()> {
		let result = async {
			// Reject any volume/network/container name Podman's regex would refuse
			// before issuing a single create, so a bad name surfaces as a clear
			// client-side error (not an opaque HTTP 500) with nothing created.
			self.validate_object_names(file)?;
			// When `x-podman-pod: true` is set, refuse any compose shape the
			// pod cannot honour (divergent networks, duplicate host ports, …)
			// with a message naming the offending service and key, before any
			// resource has been created.
			if file
				.podman_pod()
				.map_err(crate::error::ComposeError::Unsupported)?
			{
				crate::engine::pod::validate_pod_or_refuse(file)
					.map_err(crate::error::ComposeError::Unsupported)?;
			}

			let levels = crate::compose::resolve_levels(file)?;
			let active = active_profiles_set(active_profiles);
			// Which services this `up`/`create` should start. A profiled service
			// that an active service depends on is implicitly activated here,
			// the same set `config` reports, so `up` never leaves a started
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
			super::targets::validate_targets(file, target_services)?;
			let target_set = super::targets::expand_targets(file, target_services, no_deps);

			// Prefetch the project's containers once (instead of one API call per
			// replica): which names already exist, and for each the two facts
			// that decide whether it is kept or replaced (see
			// [`super::ExistingContainer`]).
			//
			// Fetched on the `--force-recreate` path too. Neither fact is
			// consulted there, but membership is what tells the progress stream
			// that a container was replaced rather than created (#1619), and a
			// forced recreate is exactly the case where that word matters.
			let mut existing: HashMap<String, super::ExistingContainer> = HashMap::new();
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
					let config_hash = entry.labels.get("podup.config-hash").cloned();
					for raw in entry.names {
						existing.insert(
							raw.trim_start_matches('/').to_string(),
							super::ExistingContainer {
								config_hash: config_hash.clone(),
								image_id: entry.image_id.clone(),
							},
						);
					}
				}
			}

			// Seed the board before any work starts. This is the whole point of
			// the phase: the resource set is knowable here, since `levels` is already
			// resolved above, and the networks and volumes come straight off the
			// compose file, while every progress event in the tree fires once
			// its work is already over. A board grown from those events would be
			// a transcript with extra steps.
			let mut resources = self.up_resources(file, &enabled, &target_set);
			// `up --build` shares one board with the build phase (#1700): the
			// image rows are seeded at the top, in `build_all`'s order, so they
			// read as the prerequisite of the network/container rows that
			// follow. The loop runs below, inside this same `up` board, so the
			// build's `Building`/`Built` verbs land on the same rows the rest
			// of `up` is drawing on.
			let build_names = if build && !self.no_build {
				up_build_targets(file, &enabled, &target_set)
			} else {
				Vec::new()
			};
			let image_tags = crate::engine::build::build_image_tags(self, file, &build_names);
			if !image_tags.is_empty() {
				let image_rows = image_tags
					.iter()
					.cloned()
					.map(|tag| (Kind::Image, tag))
					.collect::<Vec<_>>();
				resources.splice(0..0, image_rows);
			}
			crate::ui::progress::begin(resources);

			// Run the build inline on the same board, right after seeding. A
			// failed build fails `up` the way a failed network create does
			// today: the `?` propagates, the outer `end` still runs, the
			// board closes once. The build itself is unchanged: same requests,
			// same `Building n/m` verbs, same folded tail and failure replay
			// as a standalone `build`. Only the surrounding board is now the
			// one `up` already opened.
			if !build_names.is_empty() {
				self.build_images_in_session(
					file,
					&build_names,
					&crate::engine::BuildOptions::default(),
				)
				.await?;
			}

			self.create_networks(file).await?;
			self.create_volumes(file).await?;
			// Pre-create the union of inline secrets/configs once, before the
			// concurrent per-level start loop, so two services in the same level
			// can't race the non-atomic delete-then-create of a shared name.
			self.create_project_secrets(file).await?;

			// `x-podman-pod: true` opts the project into one-pod-per-project
			// semantics: ensure the pod exists (and matches the current
			// hash) before any container joins it. Recreate when the hash
			// has drifted; the recreation's `force=true` remove also drops
			// the project's containers, which the per-service loop below
			// will replace.
			let pod_enabled = file
				.podman_pod()
				.map_err(crate::error::ComposeError::Unsupported)?;
			let pod_parsed_ports: Vec<Vec<crate::ports::ParsedPort>> = if pod_enabled {
				file.services
					.values()
					.map(|s| crate::ports::parse_ports(&s.ports))
					.collect::<crate::error::Result<Vec<_>>>()?
			} else {
				Vec::new()
			};
			// A recreated pod took its member containers with it, so the list
			// above no longer describes anything; the loop below would otherwise
			// keep or start containers that are gone.
			if pod_enabled && self.ensure_pod(file, &pod_parsed_ports).await? {
				existing.clear();
			}

			// Best-effort: warm the image cache for every service this pass will
			// pull, concurrently, before the per-level walk below serializes a
			// level-2+ service's image acquisition behind the level-1 barrier.
			// A prefetch I/O miss is never fatal: `up_one_service`'s own pull
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
				&existing,
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
}

/// The services `up --build` should build, in the order `build_all` would
/// iterate them. Filters the file's services through the same profile and
/// target checks the rest of the `up` walk applies (`#1700`), then keeps
/// only those that have a `build:` block. With no explicit targets and no
/// active target set, that is every service with `build:` in the file.
/// The same "build every service with `build:`" rule the standalone `build`
/// command follows when called without arguments.
fn up_build_targets(
	file: &crate::compose::types::ComposeFile,
	enabled: &std::collections::HashSet<String>,
	target_set: &Option<std::collections::HashSet<String>>,
) -> Vec<String> {
	file.services
		.iter()
		.filter(|(name, service)| {
			if service.build.is_none() {
				return false;
			}
			if !enabled.contains(*name) {
				return false;
			}
			if let Some(set) = target_set {
				if !set.contains(*name) {
					return false;
				}
			}
			true
		})
		.map(|(name, _)| name.clone())
		.collect()
}

#[cfg(all(test, unix))]
#[path = "up_build_board_tests.rs"]
mod up_build_board_tests;
