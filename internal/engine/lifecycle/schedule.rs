//! Per-service start scheduling for `up`/`create`.
//!
//! Split from `mod.rs` to keep that file within the source line limit. The
//! scheduler is a subject of its own: it is where the difference between
//! "levels" and "a dependency graph" lives, and the two deadlock hazards it
//! avoids are documented inline rather than being folded into `run_up`.

use std::collections::{HashMap, HashSet};

use crate::compose::types::ComposeFile;
use crate::error::Result;

use super::readiness::SharedReady;
use super::{Engine, ExistingContainer};

impl Engine {
	/// Start every scheduled service, each as soon as its own dependencies are
	/// up. `levels` is used only for its membership and validation, not as a
	/// barrier.
	#[allow(clippy::too_many_arguments)]
	pub(super) async fn start_services_by_dependency(
		&self,
		levels: &[Vec<String>],
		file: &ComposeFile,
		enabled: &HashSet<String>,
		target_set: &Option<HashSet<String>>,
		existing: &HashMap<String, ExistingContainer>,
		no_recreate: bool,
		force_recreate: bool,
		start: bool,
		readiness: &HashMap<String, SharedReady<'_>>,
	) -> Result<()> {
		// Per-service scheduling, not level barriers. A service starts as soon
		// as *its own* dependencies are up, rather than waiting for every
		// service in the level ahead of it.
		//
		// The levels are still resolved, because that is what validates the
		// graph (a cycle or a missing required dependency is an error before
		// anything is created); they just no longer gate execution.
		//
		// Measured cost of the barrier on a 41-service level with one dependent
		// behind one of them: the dependent started 809ms after its dependency
		// under barriers and 129ms under docker compose's per-service DAG,
		// because a level is created with bounded concurrency and the barrier
		// made the dependent wait for the last batch rather than for what it
		// actually needed.
		//
		// Two things make this safe to do with plain futures:
		//
		// * `try_join_all` polls *every* future, so a service blocked on a
		//   dependency yields and the others make progress. `buffer_unordered`
		//   would deadlock here: it only polls the first N, and those N can
		//   all be waiting on a dependency that is queued behind them.
		// * the concurrency bound is a semaphore held only across the actual
		//   work, never across a dependency wait, for the same reason.
		//
		// A failure cancels the rest: `try_join_all` drops the other futures on
		// the first error, so a service still waiting on a dependency that will
		// now never come up is dropped rather than hanging.
		let scheduled: Vec<&str> = levels.iter().flatten().map(String::as_str).collect();
		let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(
			super::parallel::MAX_LIFECYCLE_CONCURRENCY,
		));
		let done: std::collections::HashMap<&str, tokio::sync::watch::Sender<bool>> = scheduled
			.iter()
			.map(|name| (*name, tokio::sync::watch::channel(false).0))
			.collect();

		// Borrow the shared inputs once so the closure captures references
		// rather than trying to move them per service.
		let enabled = &enabled;
		let target_set = &target_set;
		let existing = &existing;
		let readiness = &readiness;
		let started = scheduled.iter().map(|name| {
			let permits = permits.clone();
			let done = &done;
			// Only the dependencies this service actually declares, and only
			// those that are in this run's scheduled set; a service excluded by
			// profiles or an explicit target list is not going to signal.
			let deps: Vec<tokio::sync::watch::Receiver<bool>> = file
				.services
				.get(*name)
				.map(|s| s.depends_on.service_names())
				.unwrap_or_default()
				.into_iter()
				.filter_map(|d| done.get(d.as_str()).map(|tx| tx.subscribe()))
				.collect();
			async move {
				for mut rx in deps {
					// `changed()` errors only when every sender is gone, which
					// here means the run is being torn down; treat it as done and
					// let `try_join_all` surface the real error.
					while !*rx.borrow_and_update() {
						if rx.changed().await.is_err() {
							break;
						}
					}
				}
				// The permit is taken *after* the waits, so a blocked service
				// never occupies one.
				let _permit = permits.acquire().await;
				let result = self
					.up_one_service(
						name,
						file,
						enabled,
						target_set,
						existing,
						no_recreate,
						force_recreate,
						start,
						readiness,
					)
					.await;
				if result.is_ok() {
					if let Some(tx) = done.get(*name) {
						let _ = tx.send(true);
					}
				}
				result
			}
		});
		futures_util::future::try_join_all(started).await?;
		Ok(())
	}
}
