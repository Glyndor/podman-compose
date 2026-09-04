//! What a lifecycle command is about to work through, worked out before it
//! starts.
//!
//! The board needs the resource set up front, and nothing in the tree produced
//! it: every progress event fires once its own work is over, so a board grown
//! from those events would only ever show the past. This is the missing half,
//! and it is derivable — `resolve_levels` already runs before the walk begins,
//! and the networks and volumes come straight off the compose file.
//!
//! Deliberately a *prediction*, not a promise. It is computed from the compose
//! file before Podman is asked anything, so it can be wrong in both directions:
//! a service whose container already exists still gets a row (it will report
//! `Running` rather than `Started`), and a resource the file does not mention
//! is appended by the board when its event arrives. Being approximately right
//! before the work starts beats being exactly right after it finished.

use std::collections::{HashMap, HashSet};

use crate::compose::types::{ComposeFile, Service};
use crate::ui::progress::Kind;

use super::super::network::resolve_network_name;
use super::super::Engine;

/// Every container a level-walking lifecycle command will act on, in the order
/// it will act on them: the levels as that command walks them (already
/// reversed for the ones that tear down), and within a level the replicas the
/// prefetched listing found.
///
/// Built from what Podman actually has, like [`Engine::down_resources`]: a
/// service the file defines but that was never created contributes no row,
/// because a row that never moves reads as something hung. Shared by `stop`,
/// `start`, `restart`, `kill`, `pause`, `unpause` and `rm`, which all walk
/// levels over the same prefetched listing.
pub(super) fn level_container_resources(
	levels: &[Vec<String>],
	live_by_service: &HashMap<String, Vec<String>>,
) -> Vec<(Kind, String)> {
	levels
		.iter()
		.flatten()
		.filter_map(|service| live_by_service.get(service))
		.flatten()
		.map(|name| (Kind::Container, name.clone()))
		.collect()
}

impl Engine {
	/// The project networks a command will create or remove: every declared
	/// network that is not `external`, under the name Podman will show it by.
	///
	/// External networks are never created or removed by podup, so they are not
	/// work any command will do and must not appear as rows waiting to happen.
	fn network_resources(&self, file: &ComposeFile) -> Vec<(Kind, String)> {
		file.networks
			.iter()
			.filter(|(_, cfg)| !cfg.as_ref().and_then(|c| c.external).unwrap_or(false))
			.map(|(key, _)| {
				(
					Kind::Network,
					resolve_network_name(key, file, &self.project),
				)
			})
			.collect()
	}

	/// The project volumes a command will create or remove, under the names
	/// Podman will show them by. External volumes are left out for the same
	/// reason external networks are.
	fn volume_resources(&self, file: &ComposeFile) -> Vec<(Kind, String)> {
		file.volumes
			.iter()
			.filter(|(_, cfg)| !cfg.as_ref().and_then(|c| c.external).unwrap_or(false))
			.map(|(key, cfg)| {
				let name = cfg
					.as_ref()
					.and_then(|c| c.name.as_deref())
					.map(str::to_string)
					.unwrap_or_else(|| format!("{}_{key}", self.project));
				(Kind::Volume, name)
			})
			.collect()
	}

	/// Every resource an `up`/`create` pass will touch, in the order it will
	/// touch them: networks, then volumes, then containers by dependency level.
	///
	/// The order matters as much as the contents — the board's completed rows
	/// scroll away in this order, and that scrollback is the record the command
	/// leaves behind.
	pub(super) fn up_resources(
		&self,
		file: &ComposeFile,
		enabled: &HashSet<String>,
		target_set: &Option<HashSet<String>>,
	) -> Vec<(Kind, String)> {
		let mut out = self.network_resources(file);
		out.extend(self.volume_resources(file));

		// Containers in dependency order, which is the order they will start.
		// Falls back to the file's own order if the graph cannot be resolved —
		// a cycle is a real error, but it is `run_up`'s to report, and seeding
		// must not be the thing that surfaces it.
		let levels = crate::compose::resolve_levels(file)
			.unwrap_or_else(|_| vec![file.services.keys().cloned().collect::<Vec<_>>()]);
		for level in levels {
			for name in level {
				// The same two conditions `up_one_service` applies, in the same
				// order. Any drift here puts a row on the board that will never
				// move, which is worse than no row at all: a permanently
				// `Pending` line reads as something hung.
				if target_set.as_ref().is_some_and(|set| !set.contains(&name)) {
					continue;
				}
				if !enabled.contains(&name) {
					continue;
				}
				let Some(service) = file.services.get(&name) else {
					continue;
				};
				for container in self.replica_names(&name, service) {
					out.push((Kind::Container, container));
				}
			}
		}
		out
	}

	/// Every resource a `down` pass will remove, in teardown order: containers
	/// first (dependents before their dependencies, the inversion `down` itself
	/// walks), then networks, then volumes.
	///
	/// Containers come from what Podman actually has, not from the compose file:
	/// `down` removes what exists, and a file listing a service that was never
	/// created would put a row on the board that never moves.
	pub(super) fn down_resources(
		&self,
		file: &ComposeFile,
		live: &[String],
		remove_volumes: bool,
	) -> Vec<(Kind, String)> {
		let mut out: Vec<(Kind, String)> = live
			.iter()
			.map(|name| (Kind::Container, name.clone()))
			.collect();
		out.extend(self.network_resources(file));
		// Volumes survive a `down` without `-v`, so they are not work this pass
		// will do.
		if remove_volumes {
			out.extend(self.volume_resources(file));
		}
		out
	}

	/// What a one-off `run` reports before the container's own output takes
	/// over: the project networks it ensures, then the service's image when
	/// that image still has to be acquired.
	///
	/// `image_missing` is the caller's answer to "is the image absent from
	/// local storage", which is the only part of this list that a compose file
	/// cannot tell. An image already in storage produces no verb on this path,
	/// so seeding a row for it would leave a line reading `Pending` after the
	/// board closed.
	pub(super) fn run_resources(
		&self,
		file: &ComposeFile,
		service: &Service,
		image_missing: bool,
	) -> Vec<(Kind, String)> {
		let mut out = self.network_resources(file);
		if let Some(image) = service.image.as_deref().filter(|_| image_missing) {
			out.push((Kind::Image, image.to_string()));
		}
		out
	}
}

#[cfg(all(test, unix))]
#[path = "seed_tests.rs"]
mod tests;
