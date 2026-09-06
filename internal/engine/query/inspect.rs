//! Container inspection commands: `top`, `port`, and the single-service
//! `attach` command.
//!
//! The multi-service log-attach subsystem (`attach_logs`,
//! `attach_logs_with_options`, `attach_logs_with`, the `AttachOutcome` /
//! `AttachOptions` / `AttachSummary` types and the abort-path plumbing) lives
//! in `attach.rs`; this file keeps the smaller surface that doesn't share
//! those types.

use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, LogOutput, API_PREFIX};

use super::inspect_util::{
	dedup_preserving_order, is_running_status, parse_port_proto, process_table, select_replica,
};
use super::Engine;

impl Engine {
	/// Display running processes in each service container (`docker compose top`).
	///
	/// If `target_services` is empty, all services are queried.
	pub async fn top(&self, file: &ComposeFile, target_services: &[String]) -> Result<()> {
		self.top_with_options(file, target_services, false).await
	}

	/// `top` with `docker compose top`-style options: `--format json` emits a
	/// structured array of `{Container, Titles, Processes}` instead of the table.
	pub async fn top_with_options(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		json: bool,
	) -> Result<()> {
		let names: Vec<String> = if target_services.is_empty() {
			file.services.keys().cloned().collect()
		} else {
			for name in target_services {
				if !file.services.contains_key(name) {
					return Err(crate::error::ComposeError::ServiceNotFound(name.clone()));
				}
			}
			// Deduplicate repeated positionals (`top web web`) preserving order, so
			// a service's process block is not rendered twice and we avoid redundant
			// `/top` API calls, matching docker compose top.
			dedup_preserving_order(target_services)
		};

		let mut json_rows: Vec<serde_json::Value> = Vec::new();
		for name in &names {
			// Only running containers are asked for their process list, so a
			// stopped replica is skipped before the call rather than after it
			// fails: `/top` answers a non-running container with an HTTP 500, and
			// the rule below (that a non-404 must surface) is deliberate and
			// stays. Measured against `docker compose top` v5.1.3 on the same
			// Podman socket: it omits a stopped service, prints the rest and exits
			// 0 (#1250).
			for container_name in self.running_replica_names(name).await? {
				let path = format!(
					"{API_PREFIX}/containers/{}/top",
					urlencoded(&container_name),
				);
				match self
					.client
					.get_json::<crate::libpod::types::container::TopResponse>(&path)
					.await
				{
					Ok(result) if json => json_rows.push(serde_json::json!({
						"Container": container_name,
						"Titles": result.titles,
						"Processes": result.processes,
					})),
					Ok(result) => {
						// The container name is the only navigation aid when several
						// are listed, so it carries the same identity colour it has in
						// `ps` and `logs` rather than being merely bold.
						crate::ui::print_identity_header(&container_name);
						let titles = result.titles.clone().unwrap_or_default();
						let processes = result.processes.clone().unwrap_or_default();
						Self::print_process_table(&titles, &processes);
					}
					// A container removed between the listing above and this call
					// (404) is tolerated; any other failure (an unreachable socket,
					// a container that died in that same window) is a real error
					// that must surface with a non-zero exit instead of being
					// swallowed into a warning.
					Err(e) if e.is_status(404) => {
						tracing::debug!("top {container_name}: {e}")
					}
					Err(e) => return Err(ComposeError::Podman(e)),
				}
			}
		}
		if json {
			println!("{}", super::super::to_pretty_json("top.row", &json_rows)?);
		}
		Ok(())
	}

	/// Render one container's process list.
	///
	/// On `ui::Table` rather than the hand-rolled aligner it replaces: cells are
	/// escaped and columns sized in one place, so `top` stops being a third
	/// layout dialect that has to be fixed separately every time. The escaping
	/// is not incidental: these cells hold a process `argv` read out of a
	/// container, which is attacker-controlled.
	///
	/// The bookkeeping columns are dimmed so the command line is what the eye
	/// lands on. Before this, `top` styled its two header lines and left every
	/// process row flat, which on a busy container is the whole output.
	fn print_process_table(titles: &[String], processes: &[Vec<String>]) {
		if let Some(table) = process_table(titles, processes) {
			table.print();
		}
	}

	/// Print the public port for a given private port of a service container.
	///
	/// `proto` should be `"tcp"` or `"udp"`. Prints `HOST:PORT` to stdout.
	pub async fn port(
		&self,
		file: &ComposeFile,
		service_name: &str,
		private_port: &str,
		proto: &str,
	) -> Result<()> {
		self.port_with_index(file, service_name, private_port, proto, None)
			.await
	}

	/// Like [`Engine::port`] but targets a specific replica via `--index`
	/// (1-based); `None` uses the first replica.
	pub async fn port_with_index(
		&self,
		file: &ComposeFile,
		service_name: &str,
		private_port: &str,
		proto: &str,
		index: Option<u32>,
	) -> Result<()> {
		let (port, proto) = parse_port_proto(private_port, proto)?;

		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| crate::error::ComposeError::ServiceNotFound(service_name.into()))?;
		// Resolve against the containers Podman actually has, not the static
		// compose replica count: a service scaled purely via CLI `--scale` has no
		// `scale:` in the file, so the static count is 1 and would target the
		// never-created un-indexed base name. Falls back to the static names
		// when nothing is running yet; the bulk map (`#1445`) only sees what
		// Podman has, not the compose file.
		let live_by_service = self.live_project_replicas_sorted().await?;
		let live = match live_by_service.get(service_name) {
			Some(names) if !names.is_empty() => names.clone(),
			_ => self.replica_names(service_name, service),
		};
		let container_name = select_replica(live, service_name, index)?;

		let path = format!(
			"{API_PREFIX}/containers/{}/json",
			urlencoded(&container_name),
		);
		let info = match self
			.client
			.get_json::<crate::libpod::types::container::ContainerInspect>(&path)
			.await
		{
			Ok(info) => info,
			// Translate a missing container into a friendly not-found rather than
			// surfacing a raw podman 404.
			Err(e) if e.is_status(404) => {
				return Err(crate::error::ComposeError::ServiceNotFound(format!(
					"{service_name} (no running container '{container_name}')"
				)));
			}
			Err(e) => return Err(ComposeError::Podman(e)),
		};

		let key = format!("{port}/{proto}");
		let binding = info
			.network_settings
			.and_then(|ns| ns.ports.get(&key).cloned().flatten())
			.and_then(|bindings| bindings.into_iter().next());

		match binding {
			Some(b) => {
				let host = b.host_ip.as_deref().unwrap_or("0.0.0.0");
				let port = b.host_port.as_deref().unwrap_or("");
				println!("{host}:{port}");
				Ok(())
			}
			// No binding is a failure, not an empty answer. Printing a blank line
			// and exiting 0 made `HOST=$(podup port web 80)` yield an empty string
			// with a success status, so a script cannot tell "not published" from
			// "published at ''". docker compose exits 1 with a message here.
			//
			// The variant is `StreamTruncated` rather than `Unsupported` on
			// purpose: `Unsupported` renders as `unsupported feature: ...`, the
			// label reserved for compose features podup does not implement.
			// "this service does not publish that port" is not such a feature,
			// it is a property of the running service, and labelling it
			// `unsupported feature:` read as a podup limitation. The variant
			// renders the sentence verbatim under the CLI's own prefix (#1697).
			None => Err(ComposeError::PortNotPublished(format!(
				"{service_name} publishes no host port for {port}/{proto}"
			))),
		}
	}

	/// Attach to a single service container's output (`docker compose attach`).
	///
	/// Streams the first replica's stdout/stderr (follow) to this process's
	/// stdout/stderr with no prefix, until the container stops. podup never
	/// attaches STDIN (it allocates no TTY), so this is output-only.
	pub async fn attach(&self, file: &ComposeFile, service_name: &str) -> Result<()> {
		self.attach_with_index(file, service_name, None).await
	}

	/// Like [`Engine::attach`] but targets a specific replica via `--index`
	/// (1-based); `None` uses the first replica. This is what lets `attach` reach
	/// a scaled service's later replicas.
	pub async fn attach_with_index(
		&self,
		file: &ComposeFile,
		service_name: &str,
		index: Option<u32>,
	) -> Result<()> {
		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into()))?;
		// Resolve against the containers Podman actually has so a service scaled at
		// runtime (`up --scale=3` → `…-1`/`…-2`/`…-3`) attaches to a real replica
		// instead of the unsuffixed base name, which would 404. `--index`
		// (1-based) selects a specific live replica; `None` picks the
		// lowest-numbered live container for a stable choice.
		let mut live = self
			.list_project_container_names(Some(service_name))
			.await?;
		live.sort();
		let container = match index {
			Some(i) => {
				let idx = (i as usize).checked_sub(1).ok_or_else(|| {
					ComposeError::Unsupported(format!("attach: --index must be >= 1 (got {i})"))
				})?;
				live.into_iter().nth(idx).ok_or_else(|| {
					ComposeError::ServiceNotFound(format!("{service_name} (replica index {i})"))
				})?
			}
			None => live.into_iter().next().ok_or_else(|| {
				ComposeError::Unsupported(format!(
					"attach: no running container for service '{service_name}'"
				))
			})?,
		};
		let is_tty = service.tty.unwrap_or(false);

		// `docker compose attach` errors when the target is not running. Without
		// this check the libpod logs endpoint replays the *entire* history of a
		// stopped container and then ends the stream, so `attach` would print the
		// whole log and exit 0. Inspect the state first and fail closed otherwise.
		let inspect_path = format!("{API_PREFIX}/containers/{}/json", urlencoded(&container));
		let info = self
			.client
			.get_json::<crate::libpod::types::container::ContainerInspect>(&inspect_path)
			.await
			.map_err(ComposeError::Podman)?;
		let status = info.state.and_then(|s| s.status).unwrap_or_default();
		if !is_running_status(&status) {
			let shown = if status.is_empty() {
				"unknown"
			} else {
				&status
			};
			return Err(ComposeError::Unsupported(format!(
				"cannot attach to {container}: container is not running (state: {shown})"
			)));
		}

		let path = format!(
			"{API_PREFIX}/containers/{}/logs?{}",
			urlencoded(&container),
			attach_log_query(),
		);
		// A service that exists in the compose file but has no created container
		// answers 404 here; surface a friendly "service X is not running" instead
		// of leaking a raw libpod HTTP 404, mirroring the ServiceNotFound a service
		// absent from compose gets.
		let resp = match self.client.get_stream(&path).await {
			Ok(r) => r,
			Err(e) if e.is_status(404) => {
				return Err(ComposeError::NotRunning(service_name.into()))
			}
			Err(e) => return Err(ComposeError::Podman(e)),
		};
		let mut stream = if is_tty {
			crate::libpod::parse_raw(resp.into_body())
		} else {
			crate::libpod::parse_multiplexed(resp.into_body())
		};
		while let Some(msg) = stream.next().await {
			match msg {
				Ok(LogOutput::StdOut { message }) => {
					print!("{}", String::from_utf8_lossy(&message));
				}
				Ok(LogOutput::StdErr { message }) => {
					eprint!("{}", String::from_utf8_lossy(&message));
				}
				Err(_) => break,
			}
		}
		Ok(())
	}
}

/// Query string for `attach`: a live-only stdout/stderr stream. `tail=0`
/// suppresses the historical log backlog so attach shows live output (matching
/// `docker compose attach`) instead of replaying the container's whole history.
fn attach_log_query() -> &'static str {
	"stdout=true&stderr=true&follow=true&tail=0"
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
