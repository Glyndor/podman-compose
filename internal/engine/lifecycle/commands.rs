//! Lifecycle sub-commands: restart, stop, start, kill, rm, pause, unpause, run.

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};

use super::filter_services;
use super::parallel::{
	filter_levels, first_error, join_bounded, restart_service_set, retain_levels,
};
use super::targets::{stop_deadline, stop_timeout_param};
use crate::engine::Engine;
use crate::libpod::API_PREFIX;

/// Say so when a command finished having done nothing.
///
/// `start` already did this — *"no containers to start (project not created)"* —
/// and the other six lifecycle commands did not: `rm`, `stop`, `restart`, `kill`,
/// `pause` and `unpause` all exited 0 in complete silence on a project that was
/// never created, which reads exactly like success. Measured before the fix, all
/// six printed zero lines.
///
/// One helper rather than a literal per command, so the wording cannot drift into
/// seven dialects of the same sentence.
fn note_if_idle(acted: &std::sync::atomic::AtomicBool, verb: &str) {
	if !acted.load(std::sync::atomic::Ordering::Relaxed) {
		crate::ui::progress_note(&format!("no containers to {verb}"));
	}
}

impl Engine {
	/// Run a lifecycle POST against one container with a consistent outcome:
	/// success prints a `Container {container}  {done}` progress line; "already
	/// in the desired state" (304)
	/// and "no such container" (404) are idempotent no-ops; any other failure is
	/// a real error that propagates (setting a non-zero exit) instead of being
	/// swallowed into a warning. Shared by stop/start/restart/kill/pause/unpause
	/// so they all behave the same.
	///
	/// Returns whether the container was actually acted on. This is the only
	/// place that knows: `live_replica_names` falls back to the *static* compose
	/// names when nothing is running, so a command on a project that was never
	/// created still walks a full list of container names and 404s on every one
	/// of them. Six commands exited 0 in complete silence that way (#1248), and
	/// telling that apart from real work requires the answer here, not a count of
	/// names upstream.
	pub(super) async fn run_lifecycle_op(
		&self,
		path: &str,
		container: &str,
		done: &str,
	) -> Result<bool> {
		match self.client.post_empty_ok(path).await {
			Ok(()) => {
				crate::ui::progress_line("Container", container, done);
				Ok(true)
			}
			Err(e) if e.is_status(304) || e.is_status(404) || e.is_kill_of_stopped() => {
				tracing::debug!("{container}: {done} skipped ({e})");
				Ok(false)
			}
			Err(e) => Err(ComposeError::Podman(e)),
		}
	}

	/// Like [`Self::run_lifecycle_op`] but also treats a "container state
	/// improper" error (already paused / not paused / not running) as an
	/// idempotent no-op. Podman rejects `pause`/`unpause` with a 409/500 when the
	/// container is not in the expected state; docker compose treats those as
	/// no-ops, so re-pausing or unpausing a not-paused container is harmless.
	pub(super) async fn run_idempotent_state_op(
		&self,
		path: &str,
		container: &str,
		done: &str,
	) -> Result<bool> {
		match self.client.post_empty_ok(path).await {
			Ok(()) => {
				crate::ui::progress_line("Container", container, done);
				Ok(true)
			}
			Err(e) if e.is_status(304) || e.is_status(404) || e.is_state_conflict() => {
				tracing::debug!("{container}: {done} skipped ({e})");
				Ok(false)
			}
			Err(e) => Err(ComposeError::Podman(e)),
		}
	}

	/// Stop one container, escalating to an explicit `SIGKILL` if the libpod
	/// `stop` call does not complete within the grace window.
	///
	/// libpod normally `SIGKILL`s a container itself once the grace period lapses,
	/// so a healthy stop returns inside [`stop_deadline`]. If the call instead
	/// stalls (a daemon that accepts the request then never replies, or a
	/// container the server fails to reap), the bounded wait surfaces a timeout
	/// and we send `kill?signal=SIGKILL` so podup never depends solely on the
	/// server honouring `?t`. 304/404 are idempotent no-ops, as in
	/// [`run_lifecycle_op`](Self::run_lifecycle_op).
	pub(super) async fn stop_container(&self, container: &str, grace: i32) -> Result<()> {
		let path = format!(
			"{API_PREFIX}/containers/{}/stop?t={}",
			crate::libpod::urlencoded(container),
			stop_timeout_param(grace),
		);
		match self
			.client
			.post_empty_ok_within(&path, stop_deadline(grace))
			.await
		{
			Ok(()) => {
				crate::ui::progress_line("Container", container, "Stopped");
				Ok(())
			}
			Err(e) if e.is_status(304) || e.is_status(404) => {
				tracing::debug!("{container}: stop skipped ({e})");
				Ok(())
			}
			Err(e) if e.is_timeout() => {
				tracing::warn!(
					"{container}: stop did not complete within the grace window; escalating to SIGKILL"
				);
				let kill_path = format!(
					"{API_PREFIX}/containers/{}/kill?signal=SIGKILL",
					crate::libpod::urlencoded(container),
				);
				match self.client.post_empty_ok(&kill_path).await {
					Ok(()) => {
						crate::ui::progress_line(
							"Container",
							container,
							"Killed (after stop timeout)",
						);
						Ok(())
					}
					// Already gone / not running between the timeout and the kill.
					Err(e) if e.is_status(404) || e.is_status(409) => {
						tracing::debug!("{container}: SIGKILL skipped ({e})");
						Ok(())
					}
					Err(e) => Err(ComposeError::Podman(e)),
				}
			}
			Err(e) => Err(ComposeError::Podman(e)),
		}
	}

	/// Restart the named service (or all services). Dependents with a `restart` condition in `depends_on` are also restarted.
	pub async fn restart(&self, file: &ComposeFile, service_name: Option<&str>) -> Result<()> {
		let targets: Vec<String> = service_name
			.map(|s| vec![s.to_string()])
			.unwrap_or_default();
		self.restart_with_options(file, &targets, false).await
	}

	/// Restart with options. When `target_services` is empty, all services are
	/// restarted. When `no_deps` is true, dependents with a `depends_on` restart
	/// condition are NOT cascade-restarted.
	pub async fn restart_with_options(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		no_deps: bool,
	) -> Result<()> {
		// Reject unknown target names up front, like the other commands.
		super::targets::validate_targets(file, target_services)?;
		// The services to restart: the targets plus their restart-cascade
		// dependents (one hop, unless `--no-deps`). `targets` is kept only to
		// label cascade-restarts distinctly in the logs.
		let (restart_set, targets) = restart_service_set(file, target_services, no_deps);

		// Walk dependency levels in order — a dependency restarts before its
		// dependents — but restart every service *within* a level concurrently.
		let levels = retain_levels(crate::compose::resolve_levels(file)?, |n| {
			restart_set.contains(n)
		});

		// Attempt every service and surface the first error (in service order) at
		// the end rather than aborting mid-batch and leaving later services
		// unrestarted.
		let acted = std::sync::atomic::AtomicBool::new(false);
		let mut first_err: Option<ComposeError> = None;
		for level in &levels {
			let futs = level.iter().map(|name| {
				let service = &file.services[name];
				let done = if targets.contains(name) {
					"Restarted"
				} else {
					"Restarted (dependency)"
				};
				self.restart_one_service(name, service, done, &acted)
			});
			if let Some(e) = first_error(join_bounded(futs).await) {
				first_err.get_or_insert(e);
			}
		}

		if let Some(e) = first_err {
			return Err(e);
		}
		note_if_idle(&acted, "restart");
		Ok(())
	}

	/// Block until each targeted service's containers stop, printing each exit
	/// code (`docker compose wait`). Returns `RunExited` with the last non-zero
	/// code so the process exit status reflects it, mirroring docker compose.
	pub async fn wait_services(
		&self,
		file: &ComposeFile,
		target_services: &[String],
	) -> Result<()> {
		// `docker compose wait` prints each service's exit code in the order the
		// services were given on the command line (deduplicated). Only fall back to
		// dependency order when no services were named (the "all" case).
		let order = if target_services.is_empty() {
			let order = crate::compose::resolve_order(file)?;
			filter_services(file, order, &[])?
		} else {
			for name in target_services {
				if !file.services.contains_key(name) {
					return Err(ComposeError::ServiceNotFound(name.clone()));
				}
			}
			let mut seen = std::collections::HashSet::new();
			target_services
				.iter()
				.filter(|n| seen.insert(n.as_str()))
				.cloned()
				.collect::<Vec<_>>()
		};

		let mut last_nonzero = 0i64;
		for name in &order {
			// One line per service, not per replica. docker compose prints a single
			// exit code for each service it was asked to wait on, so a caller can
			// pair its input list with the output line for line; printing per
			// replica made a service scaled to 3 return 3 lines and silently broke
			// that pairing. A scaled service reports the last non-zero code it saw,
			// falling back to 0 when every replica exited cleanly — the same rule
			// the command already used for its own exit status.
			let mut service_code = 0i64;
			let mut waited = false;
			// Only wait on containers Podman actually has. The static-name fallback
			// would POST `/wait` to a never-created predicted name and surface a raw
			// HTTP 404; docker compose treats "nothing to wait on" as an idempotent
			// no-op, so a defined-but-never-created service is simply skipped (#758).
			for container_name in self
				.list_project_container_names(Some(name.as_str()))
				.await?
			{
				let path = format!(
					"{API_PREFIX}/containers/{}/wait?condition=stopped",
					crate::libpod::urlencoded(&container_name),
				);
				let code = self
					.client
					.post_empty_json_unbounded::<i64>(&path)
					.await
					.map_err(ComposeError::Podman)?;
				waited = true;
				if code != 0 {
					service_code = code;
					last_nonzero = code;
				}
			}
			// A service with no containers stays the idempotent no-op it was: it
			// contributes no line at all, rather than a spurious 0.
			if waited {
				println!("{service_code}");
			}
		}
		if last_nonzero != 0 {
			return Err(ComposeError::RunExited(last_nonzero));
		}
		Ok(())
	}

	/// Stop running containers without removing them.
	///
	/// Services are stopped in reverse dependency order. If `target_services`
	/// is empty, all services in the compose file are stopped.
	pub async fn stop(&self, file: &ComposeFile, target_services: &[String]) -> Result<()> {
		// Stop in reverse dependency order (dependents before their dependencies),
		// one level at a time, but stop every service within a level concurrently
		// so independent grace periods overlap instead of summing (#757).
		let mut levels = crate::compose::resolve_levels(file)?;
		levels.reverse();
		let levels = filter_levels(file, levels, target_services)?;

		// Enumerate only the containers Podman actually has (no static-name
		// fallback): a defined-but-never-created service is a quiet no-op rather
		// than a phantom stop. Report "stopped" solely for containers actually
		// running/paused — stopping a Created/Exited one is a harmless no-op and
		// must not claim it stopped (#876), matching docker compose.
		let acted = std::sync::atomic::AtomicBool::new(false);
		for level in &levels {
			let futs = level.iter().map(|name| {
				let grace = self.grace_period_secs(&file.services[name]);
				self.stop_one_service(name, grace, &acted)
			});
			if let Some(e) = first_error(join_bounded(futs).await) {
				return Err(e);
			}
		}
		note_if_idle(&acted, "stop");
		Ok(())
	}

	/// Start stopped containers.
	///
	/// Services are started in dependency order. If `target_services` is empty,
	/// all services in the compose file are started.
	pub async fn start(&self, file: &ComposeFile, target_services: &[String]) -> Result<()> {
		// Start in dependency order, one level at a time, but start every service
		// within a level concurrently (#757).
		let levels = crate::compose::resolve_levels(file)?;
		let levels = filter_levels(file, levels, target_services)?;

		// Only act on containers Podman actually has. Acting on the static
		// fallback names would POST `/start` to containers that were never
		// created, 404 (swallowed as a no-op), and exit 0 silently — masking that
		// the project was never created. Attempt every live container and
		// aggregate errors rather than aborting on the first.
		let any_live = std::sync::atomic::AtomicBool::new(false);
		let mut first_err: Option<ComposeError> = None;
		for level in &levels {
			let futs = level
				.iter()
				.map(|name| self.start_one_service(name, &any_live));
			if let Some(e) = first_error(join_bounded(futs).await) {
				first_err.get_or_insert(e);
			}
		}

		if let Some(e) = first_err {
			return Err(e);
		}
		// `start`'s flag answers a narrower question than the others' — whether any
		// container existed at all, not whether anything changed — so it can name
		// the cause, and the extra clause rides in the verb.
		note_if_idle(&any_live, "start (project not created)");
		Ok(())
	}

	/// Send a signal to service containers (default: `SIGKILL`).
	///
	/// If `target_services` is empty, all services are signalled.
	pub async fn kill(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		signal: &str,
	) -> Result<()> {
		// Reject an empty/whitespace-only or otherwise invalid signal before
		// issuing any request — libpod would silently treat `signal=` as SIGKILL.
		super::signal::validate_signal(signal)?;

		let levels = crate::compose::resolve_levels(file)?;
		let levels = filter_levels(file, levels, target_services)?;

		let acted = std::sync::atomic::AtomicBool::new(false);
		for level in &levels {
			let futs = level
				.iter()
				.map(|name| self.kill_one_service(name, &file.services[name], signal, &acted));
			if let Some(e) = first_error(join_bounded(futs).await) {
				return Err(e);
			}
		}
		note_if_idle(&acted, "signal");
		Ok(())
	}

	/// Remove stopped service containers.
	///
	/// When `force` is true, running containers are stopped before removal.
	/// Services are removed in reverse dependency order.
	pub async fn rm(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		force: bool,
	) -> Result<()> {
		self.rm_with_options(file, target_services, force, false)
			.await
	}

	/// Remove stopped service containers. `remove_volumes` (`-v/--volumes`) also
	/// removes anonymous volumes attached to each container.
	pub async fn rm_with_options(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		force: bool,
		remove_volumes: bool,
	) -> Result<()> {
		// Remove in reverse dependency order, one level at a time, with the
		// per-service removals in a level running concurrently (#757).
		let mut levels = crate::compose::resolve_levels(file)?;
		levels.reverse();
		let levels = filter_levels(file, levels, target_services)?;

		let acted = std::sync::atomic::AtomicBool::new(false);
		let mut first_err: Option<ComposeError> = None;
		for level in &levels {
			let futs = level.iter().map(|name| {
				self.rm_one_service(name, &file.services[name], force, remove_volumes, &acted)
			});
			if let Some(e) = first_error(join_bounded(futs).await) {
				first_err.get_or_insert(e);
			}
		}
		if let Some(e) = first_err {
			return Err(e);
		}
		note_if_idle(&acted, "remove");
		Ok(())
	}

	/// Pause running service containers (SIGSTOP).
	///
	/// If `target_services` is empty, all services are paused.
	pub async fn pause(&self, file: &ComposeFile, target_services: &[String]) -> Result<()> {
		let levels = crate::compose::resolve_levels(file)?;
		let levels = filter_levels(file, levels, target_services)?;

		// Idempotent + best-effort: re-pausing an already-paused (or stopped)
		// container is a no-op, and one state-mismatched service must not abort the
		// batch and leave the rest in an inconsistent partial state. Services in a
		// level are paused concurrently (#757).
		let acted = std::sync::atomic::AtomicBool::new(false);
		let mut first_err: Option<ComposeError> = None;
		for level in &levels {
			let futs = level.iter().map(|name| {
				self.idempotent_state_service(name, &file.services[name], "pause", "Paused", &acted)
			});
			if let Some(e) = first_error(join_bounded(futs).await) {
				first_err.get_or_insert(e);
			}
		}
		if let Some(e) = first_err {
			return Err(e);
		}
		note_if_idle(&acted, "pause");
		Ok(())
	}

	/// Resume paused service containers.
	///
	/// If `target_services` is empty, all services are unpaused.
	pub async fn unpause(&self, file: &ComposeFile, target_services: &[String]) -> Result<()> {
		let levels = crate::compose::resolve_levels(file)?;
		let levels = filter_levels(file, levels, target_services)?;

		// Idempotent + best-effort, mirroring `pause`: unpausing a not-paused
		// container is a no-op, and a single mismatch must not abort the batch.
		// Services in a level are unpaused concurrently (#757).
		let acted = std::sync::atomic::AtomicBool::new(false);
		let mut first_err: Option<ComposeError> = None;
		for level in &levels {
			let futs = level.iter().map(|name| {
				self.idempotent_state_service(
					name,
					&file.services[name],
					"unpause",
					"Unpaused",
					&acted,
				)
			});
			if let Some(e) = first_error(join_bounded(futs).await) {
				first_err.get_or_insert(e);
			}
		}
		if let Some(e) = first_err {
			return Err(e);
		}
		note_if_idle(&acted, "unpause");
		Ok(())
	}

	/// True when a container with this exact name exists (any project). Used to
	/// refuse clobbering a pre-existing container on `run --name`.
	pub(super) async fn container_exists(&self, name: &str) -> Result<bool> {
		let path = format!(
			"{API_PREFIX}/containers/{}/json",
			crate::libpod::urlencoded(name),
		);
		match self.client.get_json::<serde_json::Value>(&path).await {
			Ok(_) => Ok(true),
			Err(e) if e.is_status(404) => Ok(false),
			Err(e) => Err(ComposeError::Podman(e)),
		}
	}
}
