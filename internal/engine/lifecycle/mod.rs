//! Service lifecycle commands: up, down, start, stop, restart, kill, rm, pause, unpause, run.

mod commands;
mod down_label;
mod drop_recheck;
mod images;
mod options;
pub(in crate::engine) mod parallel;
mod pause;
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
mod up;
mod up_one;

use std::collections::HashMap;

use crate::compose::types::{ComposeFile, Service};
use crate::error::Result;
use crate::libpod::API_PREFIX;

pub use options::{RunOptions, RunOverrides};
pub use targets::validate_stop_timeout;

use super::Engine;

/// What `up` knows about a container the project already has, read once from
/// the container list before any work starts.
///
/// Two facts decide whether the container is left alone or replaced, and they
/// answer different questions. The config hash says whether the compose
/// definition changed. The image ID says whether the image the container is
/// bound to is still the one its service resolves to, which the hash cannot:
/// a rebuild, a `pull`, or a `podman tag` moves the name and leaves the hash
/// untouched. docker compose recreates on either (measured on v5.3.1: an
/// unchanged `build:` service stays `Running`; retagging its image, or
/// rebuilding it out of band, gets `Recreate`). Before #1620 podup approximated
/// the second question with "does the service have a `build:` key", which
/// recreated every build service on every `up` and never noticed a retagged
/// `image:` one.
pub(crate) struct ExistingContainer {
	/// The `podup.config-hash` label, absent on a container podup did not
	/// create or one from before the label existed. Absent never matches, so
	/// such a container is recreated.
	config_hash: Option<String>,
	/// The 64-hex ID of the image the container was created from.
	image_id: String,
	/// The `podup.service` label, when present. Carried alongside the bulk
	/// project's container list so the scale-reconciliation step in `run_up`
	/// can filter the same in-memory snapshot instead of issuing one
	/// per-service `/containers/json` against an already-fetched data set
	/// (#1747).
	service: Option<String>,
}

impl Engine {
	/// Start all services defined in the compose file, creating containers that do not exist.
	pub async fn up(&self, file: &ComposeFile) -> Result<()> {
		self.up_with_options(file, false, &[], &[], false, false, false, false)
			.await
	}

	/// Start a container by name. Used when `up` leaves an unchanged container in
	/// place but wants to ensure it is running. "Already in the desired state"
	/// (304) and "no such container" (404) are idempotent no-ops, matching
	/// [`Self::run_lifecycle_op`]; any other failure (e.g. the container's
	/// published port is now taken) propagates instead of being swallowed.
	pub(crate) async fn ensure_started(&self, container_name: &str) -> Result<()> {
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

	/// Whether the existing container named `container_name` can be left in
	/// place: its recorded config hash equals `new_hash`, and the image it is
	/// bound to is still the one `service` resolves to now.
	///
	/// The second half is what the hash cannot see, and it applies to every
	/// service rather than only to `build:` ones. A `build:` service is the
	/// common case (a rebuild moves the tag), but `up --pull always` and a
	/// `podman tag` move an `image:` service's tag exactly the same way, and
	/// docker compose recreates on both (measured on v5.3.1). One image inspect
	/// per unchanged replica is the cost; the alternative was the pre-#1620
	/// rule of recreating every `build:` service on every `up`, which destroyed
	/// the writable layer for nothing.
	///
	/// A tag that resolves to nothing, or a container without a recorded image,
	/// counts as changed: the fail-closed answer is the recreate, which is what
	/// the old rule did unconditionally.
	async fn unchanged(
		&self,
		container_name: &str,
		name: &str,
		service: &Service,
		existing: &HashMap<String, ExistingContainer>,
		new_hash: &str,
	) -> Result<bool> {
		let Some(container) = existing.get(container_name) else {
			return Ok(false);
		};
		if container.config_hash.as_deref() != Some(new_hash) {
			return Ok(false);
		}
		if container.image_id.is_empty() {
			return Ok(false);
		}
		let tag = self.service_image_tag(name, service);
		Ok(self.image_id(&tag).await?.as_deref() == Some(container.image_id.as_str()))
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
