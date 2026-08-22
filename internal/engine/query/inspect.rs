//! Container inspection commands: top, port, and log attachment.

use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, LogOutput, API_PREFIX};

use super::inspect_util::{
	dedup_preserving_order, is_running_status, parse_port_proto, process_table, select_replica,
};
use super::Engine;

/// How an attached `up` stopped streaming.
///
/// The distinction has to survive back to the caller because the four endings
/// mean different things to a script: the containers finishing on their own is
/// success, the operator pressing Ctrl-C is not, a stream that died under a
/// container still running is a failed read, and an abort triggered by a
/// container exit carries the exit code the caller wants to propagate. The
/// caller still tears the project down in every case. Reporting an ending as
/// an error from `attach` itself would short-circuit that and leave the
/// containers running, which is a worse bug than the exit code this exists to
/// fix.
///
/// `#[non_exhaustive]` since 3.0.0, so a further ending can be added without a
/// major bump. `StreamBroke` (3.3.0) and `Aborted` (#1492) are two that already
/// were.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachOutcome {
	/// Every stream ended on its own.
	StreamsEnded,
	/// SIGINT or SIGTERM arrived while streaming.
	Interrupted,
	/// At least one stream ended while its container was still running, so live
	/// output was truncated rather than finished (#1104).
	StreamBroke,
	/// `up --abort-on-container-exit` (or `--exit-code-from`, which implies it)
	/// fired: the first container to exit triggered the abort, the rest were
	/// stopped, and the field's exit code is what the CLI propagates as its
	/// process exit status. `service` is the compose service whose code was
	/// used — the first to exit, or the named `--exit-code-from` target (which
	/// may differ when the trigger was a different service, measured against
	/// docker compose v5.1.3).
	Aborted {
		/// The compose service whose exit code is being propagated.
		service: String,
		/// The exit code reported by `service`'s container, after the abort
		/// stopped everything. May be `0` (a clean exit), non-zero (a crash),
		/// or `137` (SIGKILL during the abort teardown when the named service
		/// was still running).
		exit_code: i64,
	},
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
			// Only running containers are asked for their process list, so a
			// stopped replica is skipped before the call rather than after it
			// fails: `/top` answers a non-running container with an HTTP 500, and
			// the rule below — that a non-404 must surface — is deliberate and
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
					// (404) is tolerated; any other failure — an unreachable socket,
					// a container that died in that same window — is a real error
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
		// never-created un-indexed base name. Falls back to the static names
		// when nothing is running yet — the bulk map (`#1445`) only sees what
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
			None => Err(ComposeError::Unsupported(format!(
				"no host binding for {service_name} port {port}/{proto}"
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

	/// Attach to log streams for all services with `attach: true` (the default). Streams are multiplexed to stdout with a service-name prefix.
	pub async fn attach_logs(&self, file: &ComposeFile) -> Result<AttachOutcome> {
		self.attach_logs_with_options(file, false, false, None)
			.await
	}

	/// Like [`Engine::attach_logs`] but with `up --timestamps` and
	/// `--abort-on-container-exit` / `--exit-code-from` support.
	///
	/// `timestamps` prefixes each streamed line with the libpod RFC3339 timestamp.
	///
	/// `abort_on_container_exit` (and `exit_code_from`, which implies it) makes
	/// the call return [`AttachOutcome::Aborted`] as soon as any container
	/// exits, carrying the exit code the CLI propagates. On that path the
	/// remaining containers are stopped before the function returns, so the
	/// caller does not need to call [`Engine::stop`] — and `dispatch.rs` skips
	/// its own stop call on that outcome for the same reason.
	///
	/// A service name passed as `exit_code_from` must exist in the compose
	/// file; a missing service is rejected with [`ComposeError::ServiceNotFound`]
	/// before any work happens (matching docker compose v5.1.3).
	pub async fn attach_logs_with_options(
		&self,
		file: &ComposeFile,
		timestamps: bool,
		abort_on_container_exit: bool,
		exit_code_from: Option<&str>,
	) -> Result<AttachOutcome> {
		// Reject `--exit-code-from` naming an unknown service up front. Doing this
		// before any container is created means a typo surfaces as a clear
		// "service X not found" error, matching docker compose v5.1.3.
		if let Some(target) = exit_code_from {
			if !file.services.contains_key(target) {
				return Err(ComposeError::ServiceNotFound(target.to_string()));
			}
		}

		// Carry (service, display_name, container_name, is_tty) so the log parser
		// matches the container's framing mode (TTY containers emit raw bytes;
		// non-TTY containers emit multiplexed 8-byte-header frames) and so the
		// abort path can map a stream end back to the compose service that owns
		// it without re-deriving the project prefix.
		let attached: Vec<(String, String, String, bool)> = file
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
					(name.clone(), display, cname, is_tty)
				})
			})
			.collect();

		if attached.is_empty() {
			// Nothing to stream is not an interruption.
			return Ok(AttachOutcome::StreamsEnded);
		}

		let streams: FuturesUnordered<_> = attached
			.iter()
			.map(|(svc, display, cname, is_tty)| {
				let prefix = display.clone();
				let path = format!(
					"{API_PREFIX}/containers/{}/logs?stdout=true&stderr=true&follow=true&timestamps={timestamps}",
					urlencoded(cname),
				);
				let client = &self.client;
				let is_tty = *is_tty;
				let cname = cname.clone();
				let svc = svc.clone();
				async move {
					let resp = match client.get_stream(&path).await {
						Ok(r) => r,
						Err(e) => {
							tracing::warn!("attach_logs {prefix}: {e}");
							return (svc, cname, StreamEnd::Broke);
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
								let still_running = self.container_still_running(&cname).await;
								if super::stream_broke_mid_output(still_running) {
									tracing::warn!(
										"attach {prefix}: stream ended while the container was \
										 still running [{kind}]: {e}"
									);
									return (svc, cname, StreamEnd::Broke);
								}
								tracing::warn!(
									"attach {prefix}: stream ended as the container stopped [{kind}]"
								);
								return (svc, cname, StreamEnd::ContainerStopped);
							}
						}
					}
					// Clean end of the stream — the container stopped and the
					// transport delivered its terminator.
					(svc, cname, StreamEnd::ContainerStopped)
				}
			})
			.collect();

		// Which arm wins is the whole point: `docker compose up` exits 130 on both
		// SIGINT and SIGTERM (measured against v5.1.3, not assumed — it is 130 for
		// SIGTERM too, not the 143 the signal number would suggest), and podup
		// exited 0 for both. A CI job that runs `up` in the foreground and is
		// cancelled therefore reported success.
		//
		// `FuturesUnordered` (instead of `join_all`) is what makes
		// `--abort-on-container-exit` work: we yield on the FIRST stream to
		// finish, then decide whether to keep waiting or stop the rest. With
		// `join_all` we would have to wait for every container, and a service
		// that exits cleanly could not trigger an abort while others were still
		// running.
		let phase = wait_for_phase(streams, abort_on_container_exit).await;

		match phase {
			Phase::Interrupted => Ok(AttachOutcome::Interrupted),
			Phase::AllEnded { saw_break } => {
				if saw_break {
					Ok(AttachOutcome::StreamBroke)
				} else {
					Ok(AttachOutcome::StreamsEnded)
				}
			}
			Phase::FirstExited {
				trigger_service,
				trigger_container,
			} => {
				// Stop the rest of the project. `engine.stop` is idempotent for
				// the already-stopped trigger container, so we don't need to
				// exclude it. Best effort: the priority on this path is to
				// report the exit code, not to surface a stop failure (which
				// a downstream `down` will catch anyway).
				if let Err(e) = self.stop(file, &[]).await {
					tracing::warn!(
						"abort-on-container-exit: stop after {trigger_container} exited: {e}"
					);
				}

				// Pick the exit code to propagate. With `--exit-code-from`,
				// the named service's code wins even if some other container
				// exited first — and that container may have been SIGKILLed
				// during the `stop` above (docker compose v5.1.3 prints 137
				// for that case, measured). Otherwise the trigger's code is the
				// one podup returns.
				let (service, exit_code) = match exit_code_from {
					Some(target) => {
						let target_container =
							self.first_replica_name(target, &file.services[target]);
						let code = self
							.container_exit_code(&target_container)
							.await
							.unwrap_or(0);
						(target.to_string(), code)
					}
					None => {
						let code = self
							.container_exit_code(&trigger_container)
							.await
							.unwrap_or(0);
						(trigger_service, code)
					}
				};

				Ok(AttachOutcome::Aborted { service, exit_code })
			}
		}
	}

	/// Wait for a container's exit code, returning `None` if the container is
	/// still running or the libpod call fails. Uses `/wait?condition=stopped`,
	/// which returns immediately for an already-stopped container and blocks
	/// until one is. The abort path relies on it returning the kill code (137)
	/// for a container SIGKILLed during the abort's own `stop`.
	async fn container_exit_code(&self, container_name: &str) -> Option<i64> {
		let path = format!(
			"{API_PREFIX}/containers/{}/wait?condition=stopped",
			urlencoded(container_name),
		);
		self.client
			.post_empty_json_unbounded::<i64>(&path)
			.await
			.ok()
	}
}

/// How one stream of `attach_logs_with_options` ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEnd {
	/// The stream's transport died while its container was still running — a
	/// truncated live read, not a finished one (#1104).
	Broke,
	/// The container is stopped (clean end or transport died *because* the
	/// container stopped). The abort path treats this as the trigger.
	ContainerStopped,
}

/// What the multi-stream wait returned before the outer code acts on it.
enum Phase {
	/// SIGINT/SIGTERM arrived.
	Interrupted,
	/// Every stream finished. `saw_break` is true if any one of them ended
	/// with a truncated read, which makes the outcome `StreamBroke` rather
	/// than `StreamsEnded`.
	AllEnded { saw_break: bool },
	/// `abort_on_container_exit` is set and the named stream finished while
	/// its container was stopped — the first container to exit. The outer
	/// code stops the rest and reports the exit code.
	FirstExited {
		trigger_service: String,
		trigger_container: String,
	},
}

async fn wait_for_phase(
	mut streams: FuturesUnordered<impl futures_util::Future<Output = (String, String, StreamEnd)>>,
	abort_on_container_exit: bool,
) -> Phase {
	let mut saw_break = false;
	#[cfg(unix)]
	let mut sigterm = {
		use tokio::signal::unix::{signal, SignalKind};
		signal(SignalKind::terminate()).expect("SIGTERM handler")
	};
	loop {
		#[cfg(unix)]
		{
			tokio::select! {
				biased;
				_ = tokio::signal::ctrl_c() => return Phase::Interrupted,
				_ = sigterm.recv() => return Phase::Interrupted,
				next = streams.next() => match next {
					None => return Phase::AllEnded { saw_break },
					Some((svc, cname, end)) => match end {
						StreamEnd::Broke => saw_break = true,
						StreamEnd::ContainerStopped if abort_on_container_exit => {
							return Phase::FirstExited {
								trigger_service: svc,
								trigger_container: cname,
							};
						}
						// Without `--abort-on-container-exit` a container stopping
						// mid-stream is not an event: we keep waiting for the
						// others, and the loop terminates with `AllEnded` when
						// they all finish on their own.
						StreamEnd::ContainerStopped => {}
					},
				},
			}
		}
		#[cfg(not(unix))]
		{
			match streams.next().await {
				None => return Phase::AllEnded { saw_break },
				Some((svc, cname, end)) => match end {
					StreamEnd::Broke => saw_break = true,
					StreamEnd::ContainerStopped if abort_on_container_exit => {
						return Phase::FirstExited {
							trigger_service: svc,
							trigger_container: cname,
						};
					}
					StreamEnd::ContainerStopped => {}
				},
			}
		}
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
