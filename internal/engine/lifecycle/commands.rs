//! Lifecycle sub-commands: restart, stop, start, kill, rm, pause, unpause, run.

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};

use super::drop_recheck::LifecycleGoal;
use super::parallel::{
	filter_levels, first_error, join_bounded, restart_service_set, retain_levels,
};
use super::targets::filter_services;
use super::targets::{stop_deadline, stop_timeout_param};
use crate::engine::Engine;
use crate::libpod::API_PREFIX;

/// Display width of `wait`'s NAME column.
///
/// Fixed rather than content-sized: `wait` blocks and prints each container as
/// it exits, so there is no complete set of rows to measure. Wide enough for a
/// typical `<project>-<service>-<n>`; a longer name truncates the way every
/// other capped column in the binary does.
const WAIT_NAME_WIDTH: usize = 32;

/// `wait`'s header row.
fn wait_header() -> String {
	format!("{:<WAIT_NAME_WIDTH$} EXIT", "NAME")
}

/// One `wait` result line: the container, then its exit code coloured by whether
/// it is a failure.
///
/// The name goes through `fit_cell`, so it is escaped as well as padded — a
/// container name is not podup's own string.
fn wait_row(container: &str, code: i64) -> String {
	let cell = crate::ui::fit_cell(container, WAIT_NAME_WIDTH);
	let style = if code == 0 {
		crate::ui::Style::new().dimmed()
	} else {
		crate::ui::Style::new().fg_color(Some(crate::ui::AnsiColor::Red.into()))
	};
	let coloured = crate::ui::stdout_colored();
	format!(
		"{} {}",
		crate::ui::paint(crate::ui::identity_style(container), &cell, coloured),
		crate::ui::paint(style, &code.to_string(), coloured)
	)
}

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
pub(super) fn note_if_idle(acted: &std::sync::atomic::AtomicBool, verb: &str) {
	if !acted.load(std::sync::atomic::Ordering::Relaxed) {
		crate::ui::progress_note(&format!("no containers to {verb}"));
	}
}

/// The verb a row shows while the engine is still working, from the verb it
/// will show when done. The board keeps a spinner and a clock on the row
/// between the two; a plain sink prints only the final one.
pub(super) fn working_verb(done: &str) -> &'static str {
	match done {
		"Started" => "Starting",
		"Stopped" => "Stopping",
		"Restarted" => "Restarting",
		"Killed" => "Killing",
		"Paused" => "Pausing",
		"Unpaused" => "Unpausing",
		"Removed" => "Removing",
		_ => "Working",
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
	/// place that knows: the per-replica query helpers (e.g. the bulk
	/// [`Engine::live_project_replicas_sorted`] lookup used by `exec`/`cp`/
	/// `port`/`logs`) fall back to the *static* compose names when nothing is
	/// running, so a command on a project that was never created still walks
	/// a full list of container names and 404s on every one of them. Six
	/// commands exited 0 in complete silence that way (#1248), and telling
	/// that apart from real work requires the answer here, not a count of
	/// names upstream.
	pub(super) async fn run_lifecycle_op(
		&self,
		path: &str,
		container: &str,
		done: &str,
		goal: LifecycleGoal,
	) -> Result<bool> {
		crate::ui::progress::start("Container", container, working_verb(done));
		match self.client.post_empty_ok(path).await {
			Ok(()) => {
				crate::ui::progress_line("Container", container, done);
				Ok(true)
			}
			Err(e) if e.is_status(304) || e.is_status(404) || e.is_kill_of_stopped() => {
				tracing::debug!("{container}: {done} skipped ({e})");
				crate::ui::progress_line("Container", container, "Skipped");
				Ok(false)
			}
			// The server closed before completing the response. That is not an
			// answer: the operation may have run to completion and lost only its
			// reply. Measured on Podman 6 under concurrency, where the drops land
			// on exactly these state-changing POSTs and follow a slow one — a
			// restart that burned its full stop grace, then a drop on the next
			// (#1339). It is not a client deadline (READ_TIMEOUT is 120s) and not
			// a pooled-connection race (there is no pool; every request gets a
			// fresh socket), so the transport genuinely cannot say.
			//
			// Resolve it the way `cp` and `stats` already do: ask the observable
			// the transport cannot see. If the container reached the state the
			// operation was for, it succeeded.
			Err(e) if e.is_incomplete_message() => {
				self.confirm_lost_response(container, done, goal, e).await
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
		crate::ui::progress::start("Container", container, working_verb(done));
		match self.client.post_empty_ok(path).await {
			Ok(()) => {
				crate::ui::progress_line("Container", container, done);
				Ok(true)
			}
			Err(e) if e.is_status(304) || e.is_status(404) || e.is_state_conflict() => {
				tracing::debug!("{container}: {done} skipped ({e})");
				crate::ui::progress_line("Container", container, "Skipped");
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
	///
	/// Returns `Ok(true)` when the container actually transitioned (or its
	/// state was re-confirmed via the drop-recheck path) and `Ok(false)` when
	/// libpod said it was already stopped/gone. The bool lets
	/// [`Self::stop_one_service`] keep its `acted` flag accurate now that it no
	/// longer filters by per-container state itself — the bulk container-list
	/// helper returns names without states, so the transition bit has to come
	/// from the response (#1363).
	pub(super) async fn stop_container(&self, container: &str, grace: i32) -> Result<bool> {
		let path = format!(
			"{API_PREFIX}/containers/{}/stop?t={}",
			crate::libpod::urlencoded(container),
			stop_timeout_param(grace),
		);
		crate::ui::progress::start("Container", container, "Stopping");
		match self
			.client
			.post_empty_ok_within(&path, stop_deadline(grace))
			.await
		{
			Ok(()) => {
				crate::ui::progress_line("Container", container, "Stopped");
				Ok(true)
			}
			Err(e) if e.is_status(304) || e.is_status(404) => {
				tracing::debug!("{container}: stop skipped ({e})");
				crate::ui::progress_line("Container", container, "Skipped");
				Ok(false)
			}
			// `stop` is one of the four state-changing calls the drops were
			// measured on (#1339), and it does not go through `run_lifecycle_op`,
			// so it needs the re-check on its own.
			Err(e) if e.is_incomplete_message() => {
				self.confirm_lost_response(container, "Stopped", LifecycleGoal::NotRunning, e)
					.await
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
						Ok(true)
					}
					// Already gone / not running between the timeout and the kill.
					Err(e) if e.is_status(404) || e.is_status(409) => {
						tracing::debug!("{container}: SIGKILL skipped ({e})");
						Ok(false)
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

		// Prefetch every project container once and group by service, instead of
		// one container-list round-trip per service (S+1 → 1 for the level walk;
		// #1363). The bulk helper returns all project containers, not just the
		// running ones, so a restart that lands on a stopped replica starts it.
		let live_by_service = self.live_project_replicas().await?;

		// Attempt every service and surface the first error (in service order) at
		// the end rather than aborting mid-batch and leaving later services
		// unrestarted.
		let acted = std::sync::atomic::AtomicBool::new(false);
		// Seed the board with the containers this restart will walk, in the level
		// order it walks them, before the first `Restarted` line. The set is
		// knowable here and nowhere later: every progress event on this path
		// fires once its own container is already done.
		crate::ui::progress::begin(super::seed::level_container_resources(
			&levels,
			&live_by_service,
		));
		let outcome = async {
			let mut first_err: Option<ComposeError> = None;
			for level in &levels {
				let futs = level.iter().map(|name| {
					let service = &file.services[name];
					let grace = self.grace_period_secs(service);
					let done = if targets.contains(name) {
						"Restarted"
					} else {
						"Restarted (dependency)"
					};
					let names = live_by_service.get(name).cloned().unwrap_or_default();
					self.restart_one_service(names, grace, done, &acted)
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
		// Close the board on every exit, the way `run_up` does: the region hides
		// the cursor, so an early return through it would leave the terminal
		// without a caret. `end` is idempotent.
		crate::ui::progress::end();
		outcome?;
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
		self.wait_services_with_options(file, target_services, false)
			.await
	}

	/// [`Engine::wait_services`] with `--format json`, which emits one NDJSON
	/// object per container instead of the table.
	///
	/// NDJSON rather than a trailing array because `wait` blocks: a script that
	/// only learns anything once every container has exited has been handed the
	/// answer at the one moment it is least useful. It is also the rule `stats`
	/// already follows for its streaming output.
	pub async fn wait_services_with_options(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		json: bool,
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

		// One line per container, which is the granularity the reference reports
		// at — measured against docker compose v5.1.3 with a service scaled to 3
		// replicas, it printed three lines, one per container id. The comment this
		// replaces asserted the opposite ("a single exit code for each service"),
		// and a bare `0` per service was built on it: with more than one container
		// nothing said which code belonged to which.
		//
		// What is deliberately *not* copied is the rendering. The reference prints
		// `container "<64-char hex id>" exited with status code 0` — an id nobody
		// can map back to a service, the same sentence repeated on every line, and
		// no machine path at all. Same information, said better: the container
		// name podup already has, aligned columns, and the code coloured by
		// whether it is a failure.
		let mut last_nonzero = 0i64;
		let mut printed_header = false;
		for name in &order {
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
				if code != 0 {
					last_nonzero = code;
				}
				if json {
					println!(
						"{}",
						serde_json::json!({ "Container": container_name, "ExitCode": code })
					);
				} else {
					// The header is printed lazily, with the first result: a project
					// where nothing was waited on stays the silent no-op it was,
					// rather than emitting a header over an empty table.
					if !printed_header {
						crate::ui::print_bold_header(&wait_header());
						printed_header = true;
					}
					println!("{}", wait_row(&container_name, code));
				}
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

		// Prefetch every project container once and group by service, instead of
		// one container-list round-trip per service (S+1 → 1 for the level walk;
		// #1363). The bulk helper returns every project's containers regardless
		// of state (with `all=true`); the per-container `acted` flag still has to
		// come from the `stop` response itself now that we no longer filter
		// running/paused client-side.
		let live_by_service = self.live_project_replicas().await?;

		// Report "stopped" solely for containers actually running/paused —
		// stopping a Created/Exited one is a harmless no-op and must not claim
		// it stopped (#876), matching docker compose. `stop_container` returns
		// `Ok(false)` for libpod's 304/404 idempotent no-ops, which is how this
		// flag stays accurate with the bulk listing.
		let acted = std::sync::atomic::AtomicBool::new(false);
		// The board over the containers this stop will walk, seeded in the
		// reversed level order the walk below uses.
		crate::ui::progress::begin(super::seed::level_container_resources(
			&levels,
			&live_by_service,
		));
		let outcome = async {
			for level in &levels {
				let futs = level.iter().map(|name| {
					let grace = self.grace_period_secs(&file.services[name]);
					let names = live_by_service.get(name).cloned().unwrap_or_default();
					self.stop_one_service(names, grace, &acted)
				});
				if let Some(e) = first_error(join_bounded(futs).await) {
					return Err(e);
				}
			}
			Ok(())
		}
		.await;
		crate::ui::progress::end();
		outcome?;
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

		// Prefetch every project container once and group by service, instead of
		// one container-list round-trip per service (S+1 → 1 for the level walk;
		// #1363). Only act on containers Podman actually has — acting on the
		// static fallback names would POST `/start` to containers that were never
		// created, 404 (swallowed as a no-op), and exit 0 silently, masking that
		// the project was never created. Attempt every live container and
		// aggregate errors rather than aborting on the first.
		let live_by_service = self.live_project_replicas().await?;

		let any_live = std::sync::atomic::AtomicBool::new(false);
		// The board over the containers Podman actually has, in the dependency
		// order the walk below starts them in.
		crate::ui::progress::begin(super::seed::level_container_resources(
			&levels,
			&live_by_service,
		));
		let outcome = async {
			let mut first_err: Option<ComposeError> = None;
			for level in &levels {
				let futs = level.iter().map(|name| {
					let names = live_by_service.get(name).cloned().unwrap_or_default();
					self.start_one_service(names, &any_live)
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

		// Prefetch every project container once and group by service (#1363).
		let live_by_service = self.live_project_replicas().await?;

		let acted = std::sync::atomic::AtomicBool::new(false);
		// The board over the containers this signal will reach.
		crate::ui::progress::begin(super::seed::level_container_resources(
			&levels,
			&live_by_service,
		));
		let outcome = async {
			for level in &levels {
				let futs = level.iter().map(|name| {
					let names = live_by_service.get(name).cloned().unwrap_or_default();
					self.kill_one_service(names, signal, &acted)
				});
				if let Some(e) = first_error(join_bounded(futs).await) {
					return Err(e);
				}
			}
			Ok(())
		}
		.await;
		crate::ui::progress::end();
		outcome?;
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

		// Prefetch every project container once and group by service (#1363).
		let live_by_service = self.live_project_replicas().await?;

		let acted = std::sync::atomic::AtomicBool::new(false);
		// The board over the containers this removal will walk, in the reversed
		// level order it walks them.
		crate::ui::progress::begin(super::seed::level_container_resources(
			&levels,
			&live_by_service,
		));
		let outcome = async {
			let mut first_err: Option<ComposeError> = None;
			for level in &levels {
				let futs = level.iter().map(|name| {
					let names = live_by_service.get(name).cloned().unwrap_or_default();
					self.rm_one_service(names, force, remove_volumes, &acted)
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
		note_if_idle(&acted, "remove");
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

#[cfg(test)]
#[path = "commands_wait_output_tests.rs"]
mod wait_output_tests;

#[cfg(all(test, unix))]
#[path = "commands_board_tests.rs"]
mod board_tests;
