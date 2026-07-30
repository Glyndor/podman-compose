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

use std::collections::HashSet;

use crate::compose::types::ComposeFile;
use crate::ui::progress::Kind;

use super::super::network::resolve_network_name;
use super::super::Engine;

impl Engine {
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
		let mut out = Vec::new();

		// External networks and volumes are never created by podup, so they are
		// not work this command will do and must not appear as rows waiting to
		// happen.
		for (key, cfg) in &file.networks {
			if cfg.as_ref().and_then(|c| c.external).unwrap_or(false) {
				continue;
			}
			out.push((
				Kind::Network,
				resolve_network_name(key, file, &self.project),
			));
		}
		for (key, cfg) in &file.volumes {
			let cfg = cfg.as_ref();
			if cfg.and_then(|c| c.external).unwrap_or(false) {
				continue;
			}
			let name = cfg
				.and_then(|c| c.name.as_deref())
				.map(str::to_string)
				.unwrap_or_else(|| format!("{}_{}", self.project, key));
			out.push((Kind::Volume, name));
		}

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
}

#[cfg(all(test, unix))]
#[path = "seed_tests.rs"]
mod tests;
