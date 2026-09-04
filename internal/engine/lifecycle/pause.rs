//! `pause` and `unpause`, and the resume `rm --stop` runs first.
//!
//! Split out of `commands.rs` when it passed the line limit. `unpause_paused`
//! (#1688) resumes only the paused containers, from one project listing, and
//! draws nothing when there are none; `pause` and `unpause` keep the board
//! over every container they were asked about.
use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};

use super::commands::note_if_idle;
use super::parallel::{filter_levels, first_error, join_bounded};
use crate::engine::Engine;
use crate::libpod::API_PREFIX;

impl Engine {
	/// Pause running service containers (SIGSTOP).
	///
	/// If `target_services` is empty, all services are paused.
	pub async fn pause(&self, file: &ComposeFile, target_services: &[String]) -> Result<()> {
		let levels = crate::compose::resolve_levels(file)?;
		let levels = filter_levels(file, levels, target_services)?;

		// Prefetch every project container once and group by service (#1363).
		let live_by_service = self.live_project_replicas().await?;

		// Idempotent + best-effort: re-pausing an already-paused (or stopped)
		// container is a no-op, and one state-mismatched service must not abort the
		// batch and leave the rest in an inconsistent partial state. Services in a
		// level are paused concurrently (#757).
		let acted = std::sync::atomic::AtomicBool::new(false);
		// The board over the containers this pause will reach.
		crate::ui::progress::begin(super::seed::level_container_resources(
			&levels,
			&live_by_service,
		));
		let outcome = async {
			let mut first_err: Option<ComposeError> = None;
			for level in &levels {
				let futs = level.iter().map(|name| {
					let names = live_by_service.get(name).cloned().unwrap_or_default();
					self.idempotent_state_service(names, "pause", "Paused", &acted)
				});
				if let Some(e) = first_error(join_bounded(futs).await) {
					first_err.get_or_insert(e);
				}
			}
			match first_err {
				Some(e) => Err(e),
				None => Ok(()),
			}
		}
		.await;
		crate::ui::progress::end();
		outcome?;
		note_if_idle(&acted, "pause");
		Ok(())
	}

	/// Resume paused service containers.
	///
	/// If `target_services` is empty, all services are unpaused.
	pub async fn unpause(&self, file: &ComposeFile, target_services: &[String]) -> Result<()> {
		let levels = crate::compose::resolve_levels(file)?;
		let levels = filter_levels(file, levels, target_services)?;

		// Prefetch every project container once and group by service (#1363).
		let live_by_service = self.live_project_replicas().await?;
		self.unpause_levels(levels, live_by_service).await
	}

	/// Resume only the containers that are paused, and draw nothing when none
	/// is. This is what `rm --stop` runs before stopping, since a paused
	/// container cannot be stopped: the full `unpause` would draw a
	/// board over every container and close each unpaused one as `Skipped`,
	/// then note `no containers to unpause`, for a step nobody asked for
	/// (#1688).
	pub async fn unpause_paused(
		&self,
		file: &ComposeFile,
		target_services: &[String],
	) -> Result<()> {
		let levels = crate::compose::resolve_levels(file)?;
		let levels = filter_levels(file, levels, target_services)?;
		// One listing of the project's containers, the way `unpause` itself
		// prefetches (#1363), kept to the paused ones and grouped by service.
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			self.project_label_filter_encoded(),
		);
		let entries = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;
		let mut paused_by_service: std::collections::HashMap<String, Vec<String>> =
			std::collections::HashMap::new();
		for entry in entries {
			if !entry.state.eq_ignore_ascii_case("paused") {
				continue;
			}
			let Some(service) = entry.labels.get("podup.service") else {
				continue;
			};
			if let Some(raw) = entry.names.first() {
				paused_by_service
					.entry(service.clone())
					.or_default()
					.push(raw.trim_start_matches('/').to_string());
			}
		}
		if paused_by_service.is_empty() {
			return Ok(());
		}
		let levels: Vec<Vec<String>> = levels
			.into_iter()
			.map(|level| {
				level
					.into_iter()
					.filter(|s| paused_by_service.contains_key(s))
					.collect()
			})
			.filter(|level: &Vec<String>| !level.is_empty())
			.collect();
		self.unpause_levels(levels, paused_by_service).await
	}

	async fn unpause_levels(
		&self,
		levels: Vec<Vec<String>>,
		live_by_service: std::collections::HashMap<String, Vec<String>>,
	) -> Result<()> {
		// Idempotent + best-effort, mirroring `pause`: unpausing a not-paused
		// container is a no-op, and a single mismatch must not abort the batch.
		// Services in a level are unpaused concurrently (#757).
		let acted = std::sync::atomic::AtomicBool::new(false);
		// The board over the containers this unpause will reach.
		crate::ui::progress::begin(super::seed::level_container_resources(
			&levels,
			&live_by_service,
		));
		let outcome = async {
			let mut first_err: Option<ComposeError> = None;
			for level in &levels {
				let futs = level.iter().map(|name| {
					let names = live_by_service.get(name).cloned().unwrap_or_default();
					self.idempotent_state_service(names, "unpause", "Unpaused", &acted)
				});
				if let Some(e) = first_error(join_bounded(futs).await) {
					first_err.get_or_insert(e);
				}
			}
			match first_err {
				Some(e) => Err(e),
				None => Ok(()),
			}
		}
		.await;
		crate::ui::progress::end();
		outcome?;
		note_if_idle(&acted, "unpause");
		Ok(())
	}
}
