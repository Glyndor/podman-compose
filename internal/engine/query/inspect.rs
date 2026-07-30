//! Container inspection commands: top, port, images, and log attachment.

use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::types::image::ImageInspect;
use crate::libpod::{urlencoded, LogOutput, API_PREFIX};

use super::inspect_util::{
	dedup_preserving_order, is_running_status, parse_port_proto, process_table, select_replica,
	split_repo_tag,
};
use super::Engine;

/// How an attached `up` stopped streaming.
///
/// The distinction has to survive back to the caller because the three endings
/// mean different things to a script: the containers finishing on their own is
/// success, the operator pressing Ctrl-C is not, and a stream that died under a
/// container still running is a failed read. The caller still tears the project
/// down in every case. Reporting an ending as an error from `attach` itself
/// would short-circuit that and leave the containers running, which is a worse
/// bug than the exit code this exists to fix.
///
/// `#[non_exhaustive]` since 3.0.0, so a further ending can be added without a
/// major bump. `StreamBroke` (3.3.0) is one that already was.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachOutcome {
	/// Every stream ended on its own.
	StreamsEnded,
	/// SIGINT or SIGTERM arrived while streaming.
	Interrupted,
	/// At least one stream ended while its container was still running, so live
	/// output was truncated rather than finished (#1104).
	StreamBroke,
}

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
			// `/top` API calls — matching docker compose top.
			dedup_preserving_order(target_services)
		};

		let mut json_rows: Vec<serde_json::Value> = Vec::new();
		for name in &names {
			let service = &file.services[name];
			for container_name in self.live_replica_names(name, service).await? {
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
					// A not-created container (404) is tolerated; any other failure
					// (e.g. a stopped container's HTTP 500, or an unreachable socket)
					// is a real error that must surface with a non-zero exit instead
					// of being swallowed into a warning.
					Err(e) if e.is_status(404) => {
						tracing::debug!("top {container_name}: {e}")
					}
					Err(e) => return Err(ComposeError::Podman(e)),
				}
			}
		}
		if json {
			println!(
				"{}",
				serde_json::to_string_pretty(&json_rows).unwrap_or_default()
			);
		}
		Ok(())
	}

	/// Render one container's process list.
	///
	/// On `ui::Table` rather than the hand-rolled aligner it replaces: cells are
	/// escaped and columns sized in one place, so `top` stops being a third
	/// layout dialect that has to be fixed separately every time. The escaping
	/// is not incidental — these cells hold a process `argv` read out of a
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
		// never-created un-indexed base name. `live_replica_names` falls back to
		// the static names only when nothing is running yet.
		let live = self.live_replica_names(service_name, service).await?;
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
			None => Err(ComposeError::Unsupported(format!(
				"no host binding for {service_name} port {port}/{proto}"
			))),
		}
	}

	/// List images used by each service as a table (default options).
	pub async fn images(&self, file: &ComposeFile) -> Result<()> {
		self.images_with_options(file, super::ImagesOptions::default())
			.await
	}

	/// List service images with `docker compose images`-style options:
	/// `-q/--quiet` (IDs only) and `--format` (table | json), across all services.
	/// To restrict to specific services use [`Engine::images_with_services`].
	pub async fn images_with_options(
		&self,
		file: &ComposeFile,
		opts: super::ImagesOptions,
	) -> Result<()> {
		self.images_with_services(file, &[], opts).await
	}

	/// List service images like [`Engine::images_with_options`]. When
	/// `target_services` is non-empty, only those services are listed (an unknown
	/// name is an error), matching `docker compose images [SERVICE...]`.
	pub async fn images_with_services(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		opts: super::ImagesOptions,
	) -> Result<()> {
		for name in target_services {
			if !file.services.contains_key(name) {
				return Err(ComposeError::ServiceNotFound(name.clone()));
			}
		}
		// Collect rows first so quiet/json modes can render without the header.
		let mut rows: Vec<(String, String, String, String)> = Vec::new();
		for (name, service) in &file.services {
			if !target_services.is_empty() && !target_services.iter().any(|t| t == name) {
				continue;
			}
			let image_ref = match (&service.image, &service.build) {
				(Some(img), _) => img.clone(),
				// A build-only service's image is the tag the build step produced
				// (project-scoped `{project}-{service}:latest`, or `build.tags[0]`).
				(None, Some(build)) => {
					super::super::build::primary_build_tag(&self.project, name, None, build.tags())
				}
				(None, None) => continue,
			};
			let (repo, tag) = split_repo_tag(&image_ref);
			let path = format!("{API_PREFIX}/images/{}/json", urlencoded(&image_ref));
			match self.client.get_json::<ImageInspect>(&path).await {
				Ok(img) => {
					let id = img.id.trim_start_matches("sha256:").get(..12).unwrap_or("");
					rows.push((name.clone(), repo, tag, id.to_string()));
				}
				// A 404 means the image is simply not present locally — list it with
				// an empty ID rather than silently dropping it, matching docker
				// compose. Any other error (a connection failure / unreachable
				// socket, or an HTTP 500) is a real failure that must propagate with
				// a non-zero exit rather than printing an empty table and exiting 0.
				Err(e) if e.is_status(404) => {
					tracing::debug!("images {name}: not present ({e})");
					rows.push((name.clone(), repo, tag, String::new()));
				}
				Err(e) => return Err(ComposeError::Podman(e)),
			}
		}

		if opts.quiet {
			// Deduplicate IDs so services sharing an image emit it once, like
			// docker compose images -q. Empty IDs (not-pulled) are skipped.
			let mut seen = std::collections::HashSet::new();
			for (_, _, _, id) in &rows {
				if !id.is_empty() && seen.insert(id.as_str()) {
					println!("{id}");
				}
			}
			return Ok(());
		}
		if opts.json {
			let json: Vec<_> = rows
				.iter()
				.map(|(svc, repo, tag, id)| {
					serde_json::json!({
						"Service": svc, "Repository": repo, "Tag": tag, "ID": id,
					})
				})
				.collect();
			println!(
				"{}",
				serde_json::to_string_pretty(&json).unwrap_or_default()
			);
			return Ok(());
		}

		let mut table = crate::ui::Table::new(&["SERVICE", "REPOSITORY", "TAG", "IMAGE ID"])
			.cap(0, 48)
			.cap(1, 48)
			.cap(2, 24)
			.identity_col(0);
		for (svc, repo, tag, id) in &rows {
			table.push(vec![svc.clone(), repo.clone(), tag.clone(), id.clone()]);
		}
		table.print();
		Ok(())
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

	/// Attach to log streams for all services with `attach: true` (the default). Streams are multiplexed to stdout with a service-name prefix.
	pub async fn attach_logs(&self, file: &ComposeFile) -> Result<AttachOutcome> {
		self.attach_logs_with_options(file, false).await
	}

	/// Like [`Engine::attach_logs`] but with `up --timestamps` support: when
	/// `timestamps` is set, each streamed line carries the libpod RFC3339
	/// timestamp prefix.
	pub async fn attach_logs_with_options(
		&self,
		file: &ComposeFile,
		timestamps: bool,
	) -> Result<AttachOutcome> {
		// Carry (display_name, container_name, is_tty) so the log parser matches
		// the container's framing mode: TTY containers emit raw bytes; non-TTY
		// containers emit multiplexed 8-byte-header frames.
		let attached: Vec<(String, String, bool)> = file
			.services
			.iter()
			.filter(|(_, s)| s.attach.unwrap_or(true))
			.flat_map(|(name, s)| {
				let proj_prefix = format!("{}-", self.project);
				let is_tty = s.tty.unwrap_or(false);
				self.replica_names(name, s).into_iter().map(move |cname| {
					let display = cname
						.strip_prefix(proj_prefix.as_str())
						.map(|s| s.to_string())
						.unwrap_or_else(|| cname.clone());
					(display, cname, is_tty)
				})
			})
			.collect();

		if attached.is_empty() {
			// Nothing to stream is not an interruption.
			return Ok(AttachOutcome::StreamsEnded);
		}

		let streams: Vec<_> = attached
			.iter()
			.map(|(display, cname, is_tty)| {
				let prefix = display.clone();
				let path = format!(
					"{API_PREFIX}/containers/{}/logs?stdout=true&stderr=true&follow=true&timestamps={timestamps}",
					urlencoded(cname),
				);
				let client = &self.client;
				let is_tty = *is_tty;
				let cname = cname.clone();
				async move {
					let resp = match client.get_stream(&path).await {
						Ok(r) => r,
						Err(e) => {
							tracing::warn!("attach_logs {prefix}: {e}");
							return false;
						}
					};
					// TTY containers produce raw bytes (stdout/stderr merged).
					// Non-TTY containers produce multiplexed frames with 8-byte headers.
					let mut stream = if is_tty {
						crate::libpod::parse_raw(resp.into_body())
					} else {
						crate::libpod::parse_multiplexed(resp.into_body())
					};
					while let Some(msg) = stream.next().await {
						match msg {
							Ok(LogOutput::StdOut { message }) => {
								print!("{prefix} | {}", String::from_utf8_lossy(&message));
							}
							Ok(LogOutput::StdErr { message }) => {
								eprint!("{prefix} | {}", String::from_utf8_lossy(&message));
							}
							// An attach stream lives as long as its container and
							// ends when the container stops, so a lost chunked
							// terminator is indistinguishable at the transport
							// layer from a real mid-stream break (#1104). The
							// container answers what the transport cannot: still
							// running means live output was truncated.
							//
							// This arm used to discard the error without even a
							// warning, so `up` in the foreground could lose its
							// connection to the engine and still exit 0.
							Err(e) => {
								let kind = e.stream_end_kind();
								let broke = super::stream_broke_mid_output(
									self.container_still_running(&cname).await,
								);
								if broke {
									tracing::warn!(
										"attach {prefix}: stream ended while the container was \
										 still running [{kind}]: {e}"
									);
								} else {
									tracing::warn!(
										"attach {prefix}: stream ended as the container stopped \
										 [{kind}]"
									);
								}
								return broke;
							}
						}
					}
					false
				}
			})
			.collect();

		// Which arm wins is the whole point: `docker compose up` exits 130 on both
		// SIGINT and SIGTERM (measured against v5.1.3, not assumed — it is 130 for
		// SIGTERM too, not the 143 the signal number would suggest), and podup
		// exited 0 for both. A CI job that runs `up` in the foreground and is
		// cancelled therefore reported success.
		#[cfg(unix)]
		let outcome = {
			use tokio::signal::unix::{signal, SignalKind};
			let mut sigterm = signal(SignalKind::terminate()).expect("SIGTERM handler");
			tokio::select! {
				broke = futures_util::future::join_all(streams) => {
					// One truncated stream is enough: the output the user was
					// watching is incomplete whatever the others did.
					if broke.into_iter().any(|b| b) {
						AttachOutcome::StreamBroke
					} else {
						AttachOutcome::StreamsEnded
					}
				}
				_ = tokio::signal::ctrl_c() => AttachOutcome::Interrupted,
				_ = sigterm.recv() => AttachOutcome::Interrupted,
			}
		};
		#[cfg(not(unix))]
		let outcome = tokio::select! {
			_ = futures_util::future::join_all(streams) => AttachOutcome::StreamsEnded,
			_ = tokio::signal::ctrl_c() => AttachOutcome::Interrupted,
		};

		Ok(outcome)
	}
}

/// Query string for `attach`: a live-only stdout/stderr stream. `tail=0`
/// suppresses the historical log backlog so attach shows live output (matching
/// `docker compose attach`) instead of replaying the container's whole history.
fn attach_log_query() -> &'static str {
	"stdout=true&stderr=true&follow=true&tail=0"
}

#[cfg(test)]
mod tests {
	use super::attach_log_query;

	#[test]
	fn attach_query_suppresses_log_backlog() {
		// `tail=0` means attach streams live output only, not the full history.
		let q = attach_log_query();
		assert!(q.contains("follow=true"), "got: {q}");
		assert!(q.contains("tail=0"), "got: {q}");
	}
}

#[cfg(test)]
mod attach_outcome_tests {
	use super::AttachOutcome;

	/// The two endings must stay distinguishable. They are the difference
	/// between a CI job that ran to completion and one that was cancelled, and
	/// before this existed both reported exit 0.
	#[test]
	fn the_two_endings_are_not_equal() {
		assert_ne!(AttachOutcome::StreamsEnded, AttachOutcome::Interrupted);
	}

	/// A truncated stream is its own ending, not either of the first two. Folding
	/// it into `StreamsEnded` is what let an attached `up` lose its connection to
	/// the engine and still exit 0.
	#[test]
	fn a_broken_stream_is_neither_of_the_other_two() {
		assert_ne!(AttachOutcome::StreamBroke, AttachOutcome::StreamsEnded);
		assert_ne!(AttachOutcome::StreamBroke, AttachOutcome::Interrupted);
	}
}
