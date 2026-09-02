//! Label-scoped teardown: `down` with no compose file present.
//!
//! [`Engine::down_with_options`] walks the parsed service graph, so it needs a
//! compose file. `docker compose -p NAME down` must also tear a *running*
//! project down purely from its `podup.project` label when that file is absent
//! (e.g. it was deleted, or you only have the project name). This module reaps
//! every labelled container, network, named volume and internal secret without
//! reading a single service definition.

use crate::compose::types::ComposeFile;
use crate::error::Result;
use crate::libpod::{urlencoded, API_PREFIX};

use super::Engine;

/// Shutdown grace (seconds) for the label-only teardown, used when the CLI
/// `-t/--timeout` override is unset. There is no service definition to read a
/// `stop_grace_period` from, so this falls back to docker compose's default.
const DEFAULT_STOP_GRACE_SECS: i32 = 10;

impl Engine {
	/// Tear a project down purely from its `podup.project` label, with no compose
	/// file: stop and force-remove every labelled container (and, with
	/// `remove_volumes`, their anonymous volumes), then sweep the project's
	/// networks, named volumes and internal secrets by label.
	///
	/// Mirrors `docker compose -p NAME down` against a running project whose
	/// compose file is absent. Unlike [`Engine::down_with_options`], which works
	/// through the parsed service graph, this enumerates everything by label, so
	/// it reaps all of the project's resources regardless of whether a file would
	/// parse.
	///
	/// Best-effort across every container/network/volume so one failure never
	/// leaves the rest of the teardown undone, but the first real REMOVAL
	/// failure is remembered and returned at the end instead of being swallowed
	/// into a warning — mirroring the fix applied to [`Engine::down_with_options`]
	/// (#598). A stalled or failed `stop` does NOT count towards this: the
	/// force-remove that follows SIGKILLs the container regardless (the
	/// container-removal path forces removal), so only a genuine removal failure
	/// aggregates. A 404 (already gone) stays an idempotent no-op throughout.
	///
	/// With no compose file there is no dependency graph to level, so — unlike
	/// [`Engine::down_with_options`]'s level walk — every labelled container is
	/// independent from podup's point of view and all of them tear down in one
	/// bounded concurrent batch instead of strictly sequentially.
	pub async fn down_by_label(&self, remove_volumes: bool) -> Result<()> {
		let grace = self.stop_timeout.unwrap_or(DEFAULT_STOP_GRACE_SECS);
		let mut first_err: Option<crate::error::ComposeError> = None;

		// With no compose file, the service names are not known statically — but
		// the labelled containers we are about to tear down carry `podup.service`,
		// so list them grouped by service instead of the flat name list, and
		// register the result before any teardown progress line is printed.
		// Without this every container here fell back to the per-label hash
		// instead of the sequential identity colour `ps` and the compose-file
		// `down` path use — the same class of defect fixed for `logs` (#1082).
		//
		// `set_services` registers under the project's name in the process-wide
		// identity-colour registry. Two `Engine` values alive in one process
		// (helmly-agent's fleet) key off their own `self.project`, so neither
		// registration evicts the other (#1517). `set_project` must come
		// first; without it the registration is a no-op.
		let by_service = self.live_project_replicas().await?;
		let mut service_names: Vec<String> = by_service.keys().cloned().collect();
		service_names.sort();
		crate::ui::set_project(&self.project);
		crate::ui::set_services(&service_names);

		let containers: Vec<String> = by_service.into_values().flatten().collect();
		let futs = containers.iter().map(|container_name| {
			self.teardown_one_container(container_name, grace, &[], remove_volumes)
		});
		if let Some(e) = super::parallel::first_error(super::parallel::join_bounded(futs).await) {
			first_err.get_or_insert(e);
		}

		// Networks and named volumes are reaped by label; each sweep is
		// best-effort internally and reports its own first removal failure back
		// here instead of swallowing it.
		if let Some(e) = self.remove_project_networks_by_label().await {
			first_err.get_or_insert(e);
		}
		if remove_volumes {
			if let Some(e) = self.remove_project_volumes_by_label().await {
				first_err.get_or_insert(e);
			}
		}
		// An empty file carries no declared secrets/configs, so this falls straight
		// through to the by-label orphan sweep that already backs `down`.
		self.remove_internal_secrets(&ComposeFile::default())
			.await?;

		if let Some(e) = first_err {
			return Err(e);
		}
		Ok(())
	}

	/// Remove every network carrying this project's `podup.project` label — the
	/// implicit `<project>_default`, a network whose compose key changed, or any
	/// project network when no file is present. Only podup-labelled networks
	/// match, so a user's external network is never touched. Best-effort across
	/// every network: a list failure leaves networks in place rather than
	/// aborting teardown, and one network's removal failure never blocks the
	/// rest — but the first genuine removal failure is returned to the caller
	/// (a 404 stays an idempotent no-op) so it can decide whether to aggregate
	/// it, matching [`Engine::down_by_label`]'s exit-code contract for the same
	/// class of failure (#598).
	///
	/// Shared by [`Engine::down_with_options`] and [`Engine::down_by_label`].
	pub(super) async fn remove_project_networks_by_label(
		&self,
	) -> Option<crate::error::ComposeError> {
		let list_path = format!(
			"{API_PREFIX}/networks/json?filters={}",
			self.project_network_filter_encoded(),
		);
		let Ok(nets) = self
			.client
			.get_json::<Vec<serde_json::Value>>(&list_path)
			.await
		else {
			return None;
		};
		let mut first_err = None;
		for net in nets {
			let Some(net_name) = net.get("name").and_then(|n| n.as_str()) else {
				continue;
			};
			let del = format!("{API_PREFIX}/networks/{}", urlencoded(net_name));
			// A 404 here means the object disappeared between the list and the
			// delete, so this sweep did not remove it and must not say it did.
			match self.client.delete_existed(&del).await {
				Ok(true) => crate::ui::progress_line("Network", net_name, "Removed"),
				Ok(false) => {}
				Err(e) => {
					tracing::warn!("could not remove network {net_name}: {e}");
					first_err.get_or_insert(crate::error::ComposeError::Podman(e));
				}
			}
		}
		first_err
	}

	/// Remove every named volume carrying this project's `podup.project` label.
	/// libpod's `/volumes/json` is fetched in full and filtered client-side by
	/// the label (mirroring the secret sweep) so only podup-created volumes are
	/// deleted — a user's hand-made or external volume is never touched.
	/// Best-effort across every volume: a list failure leaves volumes in place,
	/// and one volume's removal failure never blocks the rest — but the first
	/// genuine removal failure is returned to the caller (a 404 stays an
	/// idempotent no-op), matching [`Engine::remove_project_networks_by_label`].
	pub(super) async fn remove_project_volumes_by_label(
		&self,
	) -> Option<crate::error::ComposeError> {
		let path = format!("{API_PREFIX}/volumes/json");
		let Ok(vols) = self.client.get_json::<Vec<serde_json::Value>>(&path).await else {
			return None;
		};
		let mut first_err = None;
		for vol in vols {
			if !volume_owned_by(&vol, &self.project) {
				continue;
			}
			let Some(name) = vol.get("Name").and_then(|n| n.as_str()) else {
				continue;
			};
			let del = format!("{API_PREFIX}/volumes/{}", urlencoded(name));
			// A 404 here means the object disappeared between the list and the
			// delete, so this sweep did not remove it and must not say it did.
			match self.client.delete_existed(&del).await {
				Ok(true) => crate::ui::progress_line("Volume", name, "Removed"),
				Ok(false) => {}
				Err(e) => {
					tracing::warn!("could not remove volume {name}: {e}");
					first_err.get_or_insert(crate::error::ComposeError::Podman(e));
				}
			}
		}
		first_err
	}
}

/// Whether a `/volumes/json` entry carries the `podup.project=<project>` label,
/// i.e. it is a volume podup created for this project. Pure so the ownership
/// check is unit-tested without a live Podman socket.
fn volume_owned_by(vol: &serde_json::Value, project: &str) -> bool {
	vol.get("Labels")
		.and_then(|l| l.get("podup.project"))
		.and_then(|v| v.as_str())
		== Some(project)
}

#[cfg(test)]
#[path = "down_label_tests.rs"]
mod tests;
