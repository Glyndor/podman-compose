//! Teardown: `down`, and the image removal that `down --rmi` performs.
//!
//! Split from `mod.rs`, which keeps the startup half. The two directions do
//! not share code beyond the level resolution they each invert, and holding
//! them apart keeps either one readable on its own.

use crate::compose::types::ComposeFile;
use crate::engine::network::resolve_network_name;
use crate::engine::Engine;
use crate::error::Result;
use crate::libpod::API_PREFIX;

use super::parallel;

impl Engine {
	/// Stop and remove all containers for the project. Does not remove volumes unless `remove_volumes` is set.
	pub async fn down(&self, file: &ComposeFile) -> Result<()> {
		self.down_with_options(file, false).await
	}

	/// Stop and remove services in reverse dependency order. Optionally removes named volumes and orphaned containers.
	pub async fn down_with_options(&self, file: &ComposeFile, remove_volumes: bool) -> Result<()> {
		let mut levels = crate::compose::resolve_levels(file)?;
		// Teardown inverts startup: a dependent must stop before the service it
		// depends on, so the dependency levels `up` would walk front-to-back are
		// walked back-to-front here (the same inversion the other lifecycle
		// commands' level walk uses, see `parallel.rs`).
		levels.reverse();

		// Prefetch every project container once and group by service, instead of
		// one container-list round-trip per service (S+1 → 1 for the level walk).
		let live_by_service = self.live_project_replicas().await?;

		// Seed from the containers Podman actually has, walked in the same
		// reversed level order the teardown below uses, so the board predicts
		// what will happen rather than what the file describes. A service in the
		// file that was never created must not sit on the board as a row that
		// never moves.
		let live_order: Vec<String> = levels
			.iter()
			.flatten()
			.filter_map(|svc| live_by_service.get(svc))
			.flatten()
			.cloned()
			.collect();
		crate::ui::progress::begin(self.down_resources(file, &live_order, remove_volumes));

		// Best-effort across every level/container/network/volume so one failure
		// never leaves the rest of the teardown undone, but the first real
		// REMOVAL failure is remembered and returned at the end instead of being
		// swallowed into a warning — a `down` whose container/network/volume
		// removal genuinely fails (storage error, active exec session) must exit
		// non-zero, not print a warning and exit 0 (#598). A stalled or failed
		// `stop` does NOT count towards this: the force-remove below SIGKILLs the
		// container regardless (see `container_rm_path`), so only the removal
		// outcome is aggregated. A 404 (already gone) stays an idempotent no-op
		// throughout.
		//
		// Levels are walked strictly in order — every container in one level is
		// attempted before the next level starts, preserving the dependency
		// inversion above — but the containers *within* one level tear down
		// concurrently via `join_bounded`, which returns results in input
		// (service, then container) order rather than completion order. That
		// keeps "the first error" deterministic regardless of which container
		// happens to finish first: `first_error` picks the earliest in that
		// fixed order, and since levels themselves are visited in a fixed
		// sequence, only the first level with any failure can ever set
		// `first_err` — a later level's failure is never mistaken for "first".
		let mut first_err: Option<crate::error::ComposeError> = None;

		for level in &levels {
			let futs = level.iter().flat_map(|name| {
				let service = &file.services[name];
				let grace = self.grace_period_secs(service);
				// Act only on containers Podman actually has. A defined-but-never-
				// created service (or one already torn down) has no live
				// containers, so it contributes nothing here rather than
				// synthesizing predicted names and POSTing stop/rm to them —
				// those 404 and, pre-fix, leaked warnings. docker compose
				// enumerates by label and treats "nothing there" as a quiet
				// idempotent no-op (#758).
				live_by_service
					.get(name)
					.filter(|live| !live.is_empty())
					.into_iter()
					.flatten()
					.map(move |container_name| {
						self.teardown_one_container(
							container_name,
							grace,
							&service.pre_stop,
							remove_volumes,
						)
					})
			});
			if let Some(e) = parallel::first_error(parallel::join_bounded(futs).await) {
				first_err.get_or_insert(e);
			}
		}

		// Scaled replicas (`up --scale`/`scale`) carry the `podup.service` label
		// of a service still in the file, so the level walk above already swept
		// them via `live_by_service`. Orphan containers of services *removed* from
		// the file are deliberately NOT touched here: docker compose only reaps
		// them under `--remove-orphans`, which the dispatch layer handles via
		// `remove_orphans` before teardown. Removing them unconditionally here
		// made that flag a no-op.

		// `x-podman-pod`: remove the pod (and its infra container) after the
		// project's containers are gone and before the network sweep. `down`
		// without the extension is unchanged.
		if file
			.podman_pod()
			.map_err(crate::error::ComposeError::Unsupported)?
		{
			self.remove_pod().await?;
		}

		for (key, config) in &file.networks {
			let external = config.as_ref().and_then(|c| c.external).unwrap_or(false);
			if external {
				continue;
			}
			let network_name = resolve_network_name(key, file, &self.project);
			let net_path = format!(
				"{API_PREFIX}/networks/{}",
				crate::libpod::urlencoded(&network_name),
			);
			// `delete_existed`, not `delete_ok`: this loop walks the networks the
			// compose file *declares*, which is not the same set as the networks
			// that exist. `delete_ok` throws away the boolean that tells the two
			// apart, so every 404 arrived here as `Ok(())` and was announced as a
			// removal — measured on a project that had never been created,
			// `down -v` reported removing two networks and a volume, none of
			// which had ever existed. The `Err(404)` arm below was unreachable
			// for the same reason: the layer underneath had already turned the
			// 404 into a success.
			crate::ui::progress::start("Network", &network_name, "Removing");
			match self.client.delete_existed(&net_path).await {
				Ok(true) => crate::ui::progress_line("Network", &network_name, "Removed"),
				// The network was already gone (404) — nothing to do, but the row
				// has to close, or the live board leaves it spinning on
				// `Removing` forever (#1347).
				Ok(false) => crate::ui::progress_line("Network", &network_name, "Absent"),
				Err(e) => {
					tracing::warn!("could not remove network {network_name}: {e}");
					// Close the row visibly — a `down` whose removal genuinely
					// failed previously hid the failure behind a spinner (#1347).
					crate::ui::progress_line("Network", &network_name, "Failed");
					first_err.get_or_insert(crate::error::ComposeError::Podman(e));
				}
			}
		}

		// Sweep any remaining project networks by label — the implicit
		// `<project>_default` (present only when the file was normalized), or a
		// network whose compose key changed — mirroring the container sweep so
		// teardown is complete regardless of how the file was parsed. Only
		// podup-labelled networks match, so external networks are never touched.
		// This is a supplementary catch-all on top of the file-driven network
		// loop above (which already aggregates its own failures into
		// `first_err`), so a failure here is intentionally swallowed rather than
		// folded in again.
		let _ = self.remove_project_networks_by_label().await;

		if remove_volumes {
			for (key, config) in &file.volumes {
				let external = config.as_ref().and_then(|c| c.external).unwrap_or(false);
				if external {
					continue;
				}
				let volume_name = config
					.as_ref()
					.and_then(|c| c.name.as_deref())
					.map(|s| s.to_string())
					.unwrap_or_else(|| format!("{}_{}", self.project, key));
				let vol_path = format!(
					"{API_PREFIX}/volumes/{}",
					crate::libpod::urlencoded(&volume_name),
				);
				// See the network loop above: only a delete that found something
				// may be reported, and a volume is the object where a false
				// "Removed" is worst — it names data the operator believes is
				// gone.
				crate::ui::progress::start("Volume", &volume_name, "Removing");
				match self.client.delete_existed(&vol_path).await {
					Ok(true) => crate::ui::progress_line("Volume", &volume_name, "Removed"),
					// The volume was already gone (404) — nothing to do, but the
					// row has to close, or the live board leaves it spinning on
					// `Removing` forever (#1347).
					Ok(false) => crate::ui::progress_line("Volume", &volume_name, "Absent"),
					Err(e) => {
						tracing::warn!("could not remove volume {volume_name}: {e}");
						// A `down -v` whose volume removal genuinely failed
						// previously hid the failure behind a spinner (#1347).
						crate::ui::progress_line("Volume", &volume_name, "Failed");
						first_err.get_or_insert(crate::error::ComposeError::Podman(e));
					}
				}
			}
		}

		// Internal native secrets are podup-owned (not user data), so remove
		// them unconditionally — independent of `remove_volumes`.
		let secrets = self.remove_internal_secrets(file).await;

		// Close the board before returning, on every path: the region hides the
		// cursor and an early `?` would leave the terminal without one.
		crate::ui::progress::end();
		secrets?;

		if let Some(e) = first_err {
			return Err(e);
		}
		Ok(())
	}

	/// Remove the images used by the project's services (`down --rmi`). With
	/// `local_only`, only images of services that build locally (a `build:`
	/// section) are removed — matching `docker compose down --rmi local`.
	pub async fn remove_service_images(&self, file: &ComposeFile, local_only: bool) -> Result<()> {
		// Aggregate like every sibling loop in this teardown: complete the sweep,
		// then report the first real failure. Warning and returning Ok meant
		// `down --rmi` exited 0 having left images behind.
		let mut first_err: Option<crate::error::ComposeError> = None;
		for (name, service) in &file.services {
			let builds_locally = service.build.is_some();
			if local_only && !builds_locally {
				continue;
			}
			let image = match &service.image {
				Some(img) => img.clone(),
				None if builds_locally => self.service_image_tag(name, service),
				None => continue,
			};
			// Do NOT force: a force-removal cascades to every container using the
			// image — including ones owned by other compose projects that share it
			// (e.g. two stacks both on `nginx:latest`). docker compose leaves an
			// in-use image in place, so an "in use" conflict is a skip, not a
			// failure.
			let path = format!("{API_PREFIX}/images/{}", crate::libpod::urlencoded(&image),);
			match self.client.delete_ok(&path).await {
				Ok(_) => crate::ui::progress_line("Image", &image, "Removed"),
				Err(e) if e.is_status(404) => {}
				Err(e) if e.is_image_in_use() => {
					tracing::debug!("image {image} is still in use — skipping removal")
				}
				Err(e) => {
					tracing::warn!("could not remove image {image}: {e}");
					first_err.get_or_insert(crate::error::ComposeError::Podman(e));
				}
			}
		}
		first_err.map_or(Ok(()), Err)
	}
}
