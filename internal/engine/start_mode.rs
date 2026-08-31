//! The two readings `autostart --mode start` needs from a live Podman before it
//! will write a unit: whether the container exists, and whether what exists
//! still matches the compose file.
//!
//! The policy lives in `autostart`; this file only answers. Keeping the two
//! apart means the engine has no opinion about what a mismatch should cost.

use super::container::config_hash;
use super::Engine;
use crate::compose::types::{ComposeFile, Service};
use crate::error::ComposeError;
use crate::libpod::types::container::ContainerListEntry;
use crate::Result;

impl Engine {
	/// The `podup.config-hash` label Podman holds for `container`, or `None`
	/// when no container by that name exists.
	///
	/// `Some(None)` cannot happen: podup writes the label on every container it
	/// creates (`engine::container`), so a container that exists without one was
	/// created by something else, and that is reported as a missing hash rather
	/// than as a match.
	pub async fn container_config_hash(&self, container: &str) -> Result<Option<String>> {
		let path = format!(
			"{}/containers/json?all=true&filters={}",
			crate::libpod::API_PREFIX,
			self.project_label_filter_encoded(),
		);
		let entries = self
			.client
			.get_json::<Vec<ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;
		for entry in entries {
			if entry
				.names
				.iter()
				.any(|n| n.trim_start_matches('/') == container)
			{
				return Ok(entry.labels.get("podup.config-hash").cloned());
			}
		}
		Ok(None)
	}

	/// The config hash `service` renders to right now, which is what a container
	/// created from the current file would carry.
	pub fn expected_config_hash(&self, service: &Service, file: &ComposeFile) -> Result<String> {
		config_hash(service, file)
	}
}
