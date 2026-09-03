//! Pod lifecycle glue: ensure the project's pod exists and matches the
//! current compose file, recreating it when the hash differs.

use crate::compose::types::ComposeFile;
use crate::error::Result;
use crate::libpod::API_PREFIX;

use crate::engine::Engine;

impl Engine {
	/// Ensure the project's pod exists with the current hash, recreating it
	/// when it differs. Called by `up`/`create` after networks and volumes
	/// are pre-created, before any container is started.
	/// Returns `true` when the pod was recreated. A recreate removes every
	/// member container with it, so a caller that listed the project's
	/// containers before this call must forget that list.
	pub(crate) async fn ensure_pod(
		&self,
		file: &ComposeFile,
		parsed_ports: &[Vec<crate::ports::ParsedPort>],
	) -> Result<bool> {
		let desired_hash = super::pod_config_hash(parsed_ports, file);
		let inspect_path = format!(
			"{API_PREFIX}/pods/{}/json",
			crate::libpod::urlencoded(&self.project),
		);
		match self
			.client
			.get_json::<crate::libpod::types::pod::PodInspect>(&inspect_path)
			.await
		{
			// The pod exists: compare its recorded hash against the one we
			// would set now. A match means nothing changed since last `up`;
			// a mismatch means the pod's port/network/host surface drifted
			// and the pod must be recreated.
			Ok(inspect) => {
				let recorded = inspect.labels.get(super::POD_HASH_LABEL).cloned();
				if recorded.as_deref() == Some(desired_hash.as_str()) {
					return Ok(false);
				}
				crate::ui::progress::start("Pod", &self.project, "Recreating");
				// Tear down the stale pod before recreating it: removing a pod
				// with `force=true` also drops the infra container and every
				// joined container. The project containers were already removed
				// by the caller of `ensure_pod`, so this only touches the
				// infra container.
				let del_path = format!(
					"{API_PREFIX}/pods/{}?force=true",
					crate::libpod::urlencoded(&self.project),
				);
				self.client.delete_ok(&del_path).await?;
				self.create_pod_now(file, parsed_ports, &desired_hash)
					.await?;
				crate::ui::progress_line("Pod", &self.project, "Recreated");
				Ok(true)
			}
			// 404: the pod does not exist yet. Create it.
			Err(e) if e.is_status(404) => {
				crate::ui::progress::start("Pod", &self.project, "Creating");
				self.create_pod_now(file, parsed_ports, &desired_hash)
					.await?;
				crate::ui::progress_line("Pod", &self.project, "Created");
				Ok(false)
			}
			Err(e) => Err(crate::error::ComposeError::Podman(e)),
		}
	}

	async fn create_pod_now(
		&self,
		file: &ComposeFile,
		parsed_ports: &[Vec<crate::ports::ParsedPort>],
		hash: &str,
	) -> Result<()> {
		let spec = super::build_pod_spec_with_hash(&self.project, file, parsed_ports, hash);
		let path = format!("{API_PREFIX}/pods/create");
		// libpod returns `{"Id":"..."}`; we do not use the id (the pod is
		// addressed by name), but parsing it through serde keeps the wire
		// shape pinned.
		let _resp: serde_json::Value = self
			.client
			.post_json(&path, &spec)
			.await
			.map_err(crate::error::ComposeError::Podman)?;
		Ok(())
	}

	/// Remove the project's pod, if it exists. Called by `down` after the
	/// project's containers are removed.
	pub(crate) async fn remove_pod(&self) -> Result<()> {
		let path = format!(
			"{API_PREFIX}/pods/{}?force=true",
			crate::libpod::urlencoded(&self.project),
		);
		match self.client.delete_existed(&path).await {
			Ok(true) => crate::ui::progress_line("Pod", &self.project, "Removed"),
			Ok(false) => crate::ui::progress_line("Pod", &self.project, "Absent"),
			Err(e) => {
				tracing::warn!("could not remove pod {}: {e}", self.project);
				crate::ui::progress_line("Pod", &self.project, "Failed");
			}
		}
		// `down --remove-orphans` sweeps by the `podup.project` label and is
		// already covered by `remove_project_pods_by_label` in this module.
		Ok(())
	}
}

impl Engine {
	/// Sweep every pod carrying the project's `podup.project` label, a pod
	/// left behind by a crashed `up`, or any other pod the project owns.
	/// Mirrors the network/volume sweeps `down` already does by label.
	pub(crate) async fn remove_project_pods_by_label(&self) {
		let list_path = format!(
			"{API_PREFIX}/pods/json?filters={}",
			self.project_label_filter_encoded(),
		);
		let Ok(pods) = self
			.client
			.get_json::<Vec<serde_json::Value>>(&list_path)
			.await
		else {
			return;
		};
		for pod in pods {
			let Some(name) = pod.get("Name").and_then(|n| n.as_str()) else {
				continue;
			};
			if name == self.project {
				// The current project's pod was already handled (or attempted)
				// by `remove_pod`; do not double-report its removal here.
				continue;
			}
			let del = format!(
				"{API_PREFIX}/pods/{}?force=true",
				crate::libpod::urlencoded(name),
			);
			match self.client.delete_existed(&del).await {
				Ok(true) => crate::ui::progress_line("Pod", name, "Removed"),
				Ok(false) => {}
				Err(e) => tracing::warn!("could not remove pod {name}: {e}"),
			}
		}
	}
}
