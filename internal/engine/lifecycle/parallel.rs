//! Intra-level parallelism for the lifecycle commands.
//!
//! The order resolver groups services into dependency *levels*
//! ([`crate::compose::resolve_levels`]): every service in one level has its
//! `depends_on` satisfied by an earlier level, so services *within* a level have
//! no ordering between them and can be acted on concurrently. The whole-project
//! lifecycle commands (stop/start/restart/kill/rm/pause/unpause/down) walk the
//! levels in order — preserving the cross-level dependency ordering — but
//! dispatch each level's per-service (or, for teardown, per-container)
//! operations in parallel instead of strictly serially, so a restart/stop/down
//! of many independent services no longer serializes every grace period (#757).
//! This mirrors what the `up`/`create` path already does.

use std::collections::HashSet;
use std::sync::Arc;

use crate::compose::types::{ComposeFile, LifecycleHook};
use crate::engine::Engine;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::drop_recheck::LifecycleGoal;
use super::targets::{stop_deadline, stop_timeout_param};

/// Upper bound on the number of same-level services a lifecycle command acts on
/// concurrently. Services within a dependency level have no ordering between
/// them, so they run in parallel; the cap keeps a very wide compose file from
/// opening an unbounded number of simultaneous libpod connections at once.
pub(in crate::engine) const MAX_LIFECYCLE_CONCURRENCY: usize = 16;

/// Run a batch of independent per-service futures concurrently, bounded by
/// [`MAX_LIFECYCLE_CONCURRENCY`], and return their outputs in the *input*
/// order (not completion order) so error selection and reporting stay
/// deterministic regardless of which service happens to finish first.
/// Generic over the output type so both a fallible per-service unit
/// (`Result<()>`, reduced via [`first_error`]) and a best-effort one that
/// never fails (`()`, e.g. the image-prefetch stage) share this one bounded
/// dispatcher.
///
/// Not reachable from every stage that wants a fan-out: this is built on
/// `buffer_unordered`, whose `FuturesUnordered` is `Send` only if its future is
/// `Send` for *every* lifetime. A caller whose futures borrow `&self` therefore
/// acquires a higher-ranked bound that propagates out to its own callers, which
/// is why the secret pre-creation stage (#1219) chunks `join_all` against
/// [`MAX_LIFECYCLE_CONCURRENCY`] instead of calling this. The cap is shared; the
/// dispatcher is not.
pub(super) async fn join_bounded<F, T>(futs: impl IntoIterator<Item = F>) -> Vec<T>
where
	F: std::future::Future<Output = T>,
	T: Send,
{
	use futures_util::stream::StreamExt;
	let mut indexed: Vec<(usize, T)> = futures_util::stream::iter(
		futs.into_iter()
			.enumerate()
			.map(|(i, fut)| async move { (i, fut.await) }),
	)
	.buffer_unordered(MAX_LIFECYCLE_CONCURRENCY)
	.collect()
	.await;
	indexed.sort_by_key(|(i, _)| *i);
	indexed.into_iter().map(|(_, r)| r).collect()
}

/// Reduce a level's per-service results to the first error in service order, so
/// one failing service is still reported clearly while the rest of the level is
/// allowed to complete.
pub(in crate::engine) fn first_error(results: Vec<Result<()>>) -> Option<ComposeError> {
	results.into_iter().find_map(Result::err)
}

/// Filter each dependency level down to `target_services`, dropping levels that
/// end up empty. An empty target list keeps every level. Returns an error if any
/// requested name is not in the file, matching [`super::targets::filter_services`]
/// (and docker compose's "no such service").
pub(super) fn filter_levels(
	file: &ComposeFile,
	levels: Vec<Vec<String>>,
	target_services: &[String],
) -> Result<Vec<Vec<String>>> {
	for name in target_services {
		if !file.services.contains_key(name) {
			return Err(ComposeError::ServiceNotFound(name.clone()));
		}
	}
	if target_services.is_empty() {
		return Ok(levels);
	}
	let set: HashSet<&str> = target_services.iter().map(|s| s.as_str()).collect();
	Ok(retain_levels(levels, |n| set.contains(n)))
}

/// Keep only the level entries matching `keep`, dropping levels left empty.
pub(super) fn retain_levels(
	levels: Vec<Vec<String>>,
	keep: impl Fn(&str) -> bool,
) -> Vec<Vec<String>> {
	levels
		.into_iter()
		.map(|level| {
			level
				.into_iter()
				.filter(|n| keep(n.as_str()))
				.collect::<Vec<_>>()
		})
		.filter(|level| !level.is_empty())
		.collect()
}

/// Compute the set of services a `restart` should act on, plus the explicit
/// target subset (used only to label cascade-restarts distinctly in the logs).
///
/// With no targets every service is restarted. With targets, the set is the
/// targets plus — unless `no_deps` — every service whose `depends_on` carries a
/// `restart: true` condition pointing at one of the targets (one hop, matching
/// the previous serial implementation).
pub(super) fn restart_service_set(
	file: &ComposeFile,
	target_services: &[String],
	no_deps: bool,
) -> (Arc<HashSet<String>>, Arc<HashSet<String>>) {
	if target_services.is_empty() {
		// No targets means "every service is both a target and a member of
		// the full restart set". The two `Arc`s share the same `HashSet` so
		// only one `HashSet` is built and only one `Arc::clone` is paid;
		// callers read both as immutable (`contains()`), so the aliasing is
		// safe. The previous code wrote `(all.clone(), all)` inline (#1364).
		let all: Arc<HashSet<String>> = Arc::new(file.services.keys().cloned().collect());
		(all.clone(), all)
	} else {
		let targets: Arc<HashSet<String>> = Arc::new(target_services.iter().cloned().collect());
		let mut full: HashSet<String> = targets.as_ref().clone();
		if !no_deps {
			for (dep_name, dep_service) in &file.services {
				if targets
					.iter()
					.any(|t| dep_service.depends_on.restart_for(t))
				{
					full.insert(dep_name.clone());
				}
			}
		}
		(Arc::new(full), targets)
	}
}

impl Engine {
	/// Stop a single service's live containers, as one unit of work in a
	/// concurrent level. See [`Engine::stop`]. Takes its container names from
	/// the bulk project listing (`live_project_replicas`); the per-container
	/// `acted` flag still has to come from [`Self::stop_container`] itself
	/// because the bulk helper returns names without states (#1363).
	pub(super) async fn stop_one_service(
		&self,
		container_names: Vec<String>,
		grace: i32,
		acted: &std::sync::atomic::AtomicBool,
	) -> Result<()> {
		let mut first_err: Option<ComposeError> = None;
		for container_name in container_names {
			match self.stop_container(&container_name, grace).await {
				Ok(true) => acted.store(true, std::sync::atomic::Ordering::Relaxed),
				Ok(false) => {
					tracing::debug!("{container_name}: not running — stop is a no-op");
				}
				Err(e) => {
					first_err.get_or_insert(e);
				}
			}
		}
		first_err.map_or(Ok(()), Err)
	}

	/// Start a single service's live containers, recording in `any_live` whether
	/// the service had any container to act on. See [`Engine::start`].
	pub(super) async fn start_one_service(
		&self,
		container_names: Vec<String>,
		any_live: &std::sync::atomic::AtomicBool,
	) -> Result<()> {
		if container_names.is_empty() {
			return Ok(());
		}
		any_live.store(true, std::sync::atomic::Ordering::Relaxed);
		let mut first_err: Option<ComposeError> = None;
		for container_name in container_names {
			let path = format!(
				"{API_PREFIX}/containers/{}/start",
				urlencoded(&container_name),
			);
			if let Err(e) = self
				.run_lifecycle_op(&path, &container_name, "Started", LifecycleGoal::Running)
				.await
			{
				first_err.get_or_insert(e);
			}
		}
		first_err.map_or(Ok(()), Err)
	}

	/// Restart a single service's live containers. `done` is the log verb
	/// (`restarted` for a direct target, `cascade-restarted` for a dependent).
	pub(super) async fn restart_one_service(
		&self,
		container_names: Vec<String>,
		grace: i32,
		done: &str,
		acted: &std::sync::atomic::AtomicBool,
	) -> Result<()> {
		let mut first_err: Option<ComposeError> = None;
		for container_name in container_names {
			// Single atomic restart (no visible stopped window) instead of a
			// stop+start round-trip.
			let restart_path = format!(
				"{API_PREFIX}/containers/{}/restart?t={}",
				urlencoded(&container_name),
				stop_timeout_param(grace),
			);
			match self
				.run_lifecycle_op(&restart_path, &container_name, done, LifecycleGoal::Running)
				.await
			{
				Ok(true) => acted.store(true, std::sync::atomic::Ordering::Relaxed),
				Ok(false) => {}
				Err(e) => {
					first_err.get_or_insert(e);
				}
			}
		}
		first_err.map_or(Ok(()), Err)
	}

	/// Send `signal` to a single service's live containers. See [`Engine::kill`].
	pub(super) async fn kill_one_service(
		&self,
		container_names: Vec<String>,
		signal: &str,
		acted: &std::sync::atomic::AtomicBool,
	) -> Result<()> {
		let mut first_err: Option<ComposeError> = None;
		for container_name in container_names {
			let path = format!(
				"{API_PREFIX}/containers/{}/kill?signal={}",
				urlencoded(&container_name),
				urlencoded(signal),
			);
			match self
				.run_lifecycle_op(&path, &container_name, "Killed", LifecycleGoal::NotRunning)
				.await
			{
				Ok(true) => acted.store(true, std::sync::atomic::Ordering::Relaxed),
				Ok(false) => {}
				Err(e) => {
					first_err.get_or_insert(e);
				}
			}
		}
		first_err.map_or(Ok(()), Err)
	}

	/// Remove a single service's containers. See [`Engine::rm_with_options`].
	pub(super) async fn rm_one_service(
		&self,
		container_names: Vec<String>,
		force: bool,
		remove_volumes: bool,
		acted: &std::sync::atomic::AtomicBool,
	) -> Result<()> {
		let mut first_err: Option<ComposeError> = None;
		for container_name in container_names {
			let force_str = if force { "true" } else { "false" };
			let path = format!(
				"{API_PREFIX}/containers/{}?force={force_str}&v={remove_volumes}",
				urlencoded(&container_name),
			);
			match self.client.delete_existed(&path).await {
				// Only report a removal that actually happened — a phantom
				// (never-created) container 404s and must not be logged as
				// "removed".
				Ok(true) => {
					acted.store(true, std::sync::atomic::Ordering::Relaxed);
					crate::ui::progress_line("Container", &container_name, "Removed");
				}
				Ok(false) => {}
				// Without `--force`, a running container 409s. docker compose rm
				// skips running containers rather than aborting, so warn and keep
				// going (later stopped containers still get removed).
				Err(e) if !force && e.is_status(409) => {
					tracing::warn!(
						"{container_name} is running — skipping (pass -f to force removal)"
					);
				}
				Err(e) => {
					first_err.get_or_insert(ComposeError::Podman(e));
				}
			}
		}
		first_err.map_or(Ok(()), Err)
	}

	/// Pause or unpause a single service's live containers, treating a state
	/// mismatch as an idempotent no-op. `endpoint` is `pause`/`unpause`.
	pub(super) async fn idempotent_state_service(
		&self,
		container_names: Vec<String>,
		endpoint: &str,
		done: &str,
		acted: &std::sync::atomic::AtomicBool,
	) -> Result<()> {
		let mut first_err: Option<ComposeError> = None;
		for container_name in container_names {
			let path = format!(
				"{API_PREFIX}/containers/{}/{endpoint}",
				urlencoded(&container_name),
			);
			match self
				.run_idempotent_state_op(&path, &container_name, done)
				.await
			{
				Ok(true) => acted.store(true, std::sync::atomic::Ordering::Relaxed),
				Ok(false) => {}
				Err(e) => {
					first_err.get_or_insert(e);
				}
			}
		}
		first_err.map_or(Ok(()), Err)
	}

	/// Tear down one already-known-live container: run its `pre_stop` hooks (if
	/// any), a best-effort stop bounded by `grace`, then a forced removal. One
	/// unit of work in a concurrent teardown level/batch — shared by
	/// [`super::Engine::down_with_options`] (per dependency level) and
	/// [`super::Engine::down_by_label`] (one label-scoped batch, no dependency
	/// graph to level).
	///
	/// A stalled or failed `stop` is never surfaced as an error here — the
	/// forced removal that follows SIGKILLs the container regardless of how
	/// `stop` went (`container_rm_path` always passes `force=true`), so only a
	/// genuine removal failure propagates. A 404 (container already gone) is an
	/// idempotent no-op at every step. This preserves the pre-parallel `down`
	/// error semantics (#598) byte-for-byte; only the dispatch became
	/// concurrent.
	pub(super) async fn teardown_one_container(
		&self,
		container_name: &str,
		grace: i32,
		pre_stop: &[LifecycleHook],
		remove_volumes: bool,
	) -> Result<()> {
		crate::ui::progress::start("Container", container_name, "Stopping");
		for hook in pre_stop {
			if let Err(e) = self.run_lifecycle_hook(container_name, hook).await {
				tracing::debug!("pre_stop hook {container_name}: {e}");
			}
		}

		// Bound the stop by the grace window so a container ignoring SIGTERM
		// does not pin recreation for the full client READ_TIMEOUT; the
		// force-remove below SIGKILLs it regardless.
		let stop_path = format!(
			"{API_PREFIX}/containers/{}/stop?t={}",
			urlencoded(container_name),
			stop_timeout_param(grace),
		);
		// A 404 (container already gone, or a profile-gated service that was
		// never created) is an idempotent no-op here, exactly as the network and
		// volume removal arms treat it — not a warning. A stalled or failed stop
		// is not fatal either: the force-remove just below SIGKILLs the
		// container regardless, so its outcome is logged but never returned as
		// an error — only a genuine removal failure is.
		if let Err(e) = self
			.client
			.post_empty_ok_within(&stop_path, stop_deadline(grace))
			.await
		{
			if !e.is_status(404) {
				tracing::warn!("could not stop {container_name}: {e}");
			}
		}

		let rm_path = super::container_rm_path(container_name, remove_volumes);
		match self.client.delete_ok(&rm_path).await {
			Ok(()) => {
				crate::ui::progress_line("Container", container_name, "Removed");
				Ok(())
			}
			// The container was already gone (404) — nothing to do, but the row
			// has to close, or the live board leaves it spinning on `Stopping`
			// forever (#1347).
			Err(e) if e.is_status(404) => {
				crate::ui::progress_line("Container", container_name, "Absent");
				Ok(())
			}
			// The other state-changing call the drops were measured on (#1339).
			// `Gone` rather than `NotRunning`: a stopped-but-present container
			// would satisfy the latter and read a failed removal as a success.
			Err(e) if e.is_incomplete_message() => self
				.confirm_lost_response(container_name, "Removed", LifecycleGoal::Gone, e)
				.await
				.map(|_| ()),
			Err(e) => {
				tracing::warn!("could not remove {container_name}: {e}");
				// A `down` whose container removal genuinely failed previously
				// hid the failure behind a spinner (#1347).
				crate::ui::progress_line("Container", container_name, "Failed");
				Err(ComposeError::Podman(e))
			}
		}
	}
}

#[cfg(test)]
#[path = "parallel_tests.rs"]
mod tests;
