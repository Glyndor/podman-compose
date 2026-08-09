//! Query and observation commands: ps, logs, exec, pull, remove_orphans.

use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, LogOutput, API_PREFIX};

use super::Engine;

mod exec;
mod exec_interactive;
mod images;
mod inspect;
mod inspect_util;
mod log_prefix;
mod ps;
pub(crate) mod terminal;

pub use ps::{PsDisplayOptions, PsFilterOptions, PsOptions};

pub use exec::ExecOptions;
pub(crate) use exec::{stdin_is_terminal, stdout_is_terminal};
use log_prefix::LinePrefixer;

pub use inspect::AttachOutcome;

/// Options for [`Engine::images_with_options`].
#[derive(Default)]
pub struct ImagesOptions {
	/// Print only image IDs, `-q/--quiet`.
	pub quiet: bool,
	/// Emit JSON instead of the table, `--format json`.
	pub json: bool,
}

/// Default for `podup logs` when the user does not pass `--tail`: show the last
/// 100 lines. `docker compose logs` defaults to "all"; podup's bounded default
/// keeps the inspection case ("what just happened?") from flooding the
/// terminal and stops CI scripts that capture `podup logs` from silently
/// missing errors that landed before the window. Pass `--tail all` to opt
/// back into the previous behaviour.
pub const DEFAULT_LOG_TAIL: &str = "100";

/// Options for [`Engine::logs_with_options`], mirroring `docker compose logs`.
#[derive(Default)]
pub struct LogsOptions {
	/// Follow log output, `-f/--follow`.
	pub follow: bool,
	/// Number of lines to show from the end, `-n/--tail` (`None` = all).
	pub tail: Option<String>,
	/// Show logs since a timestamp/relative time, `--since`.
	pub since: Option<String>,
	/// Show logs until a timestamp/relative time, `--until`.
	pub until: Option<String>,
	/// Prefix each line with an RFC3339 timestamp, `-t/--timestamps`.
	pub timestamps: bool,
}

/// Prefix-display options for [`Engine::logs_with_display`] (`docker compose
/// logs --no-color` / `--no-log-prefix`). Kept off the frozen [`LogsOptions`]
/// struct so the published library API stays stable across minors.
#[derive(Default)]
pub struct LogsDisplay {
	/// Produce monochrome output (no colour in the prefix), `--no-color`.
	pub no_color: bool,
	/// Do not print the `{service} | ` prefix, `--no-log-prefix`.
	pub no_log_prefix: bool,
}

/// Validate the `--tail`/`--since`/`--until` values client-side so a typo is
/// rejected with a clear local message instead of a raw podman HTTP 400. `tail`
/// must be `all` or a non-negative integer; `since`/`until` must be a Unix
/// timestamp or a Go-style duration (e.g. `10m`, `1h30m`) or an RFC3339-ish
/// timestamp. Pure so it is unit-tested.
fn validate_log_filters(opts: &LogsOptions) -> Result<()> {
	if let Some(tail) = &opts.tail {
		if tail != "all" && tail.parse::<u64>().is_err() {
			return Err(ComposeError::Unsupported(format!(
				"invalid --tail value {tail:?}: expected a non-negative integer or 'all'"
			)));
		}
	}
	for (flag, value) in [("--since", &opts.since), ("--until", &opts.until)] {
		if let Some(v) = value {
			if !is_valid_log_time(v) {
				return Err(ComposeError::Unsupported(format!(
					"invalid {flag} value {v:?}: expected a duration (e.g. 10m, 1h30m), a Unix \
					 timestamp, or an RFC3339 time"
				)));
			}
		}
	}
	Ok(())
}

/// Whether a `--since`/`--until` value is a plausible duration, Unix timestamp,
/// or timestamp string. Conservative: rejects obvious garbage (`abc`) while
/// accepting the forms podman understands.
fn is_valid_log_time(v: &str) -> bool {
	if v.is_empty() {
		return false;
	}
	// Unix timestamp (optionally fractional).
	if v.parse::<f64>().is_ok() {
		return true;
	}
	// Go-style duration: digit-run + unit, repeated (e.g. 1h30m, 90s, 500ms).
	if is_go_duration(v) {
		return true;
	}
	// Timestamp-ish: starts with a 4-digit year and contains only the characters
	// an RFC3339/date string uses. The server does the precise parse; this just
	// blocks free-form garbage.
	let bytes = v.as_bytes();
	bytes.len() >= 4
		&& bytes[..4].iter().all(u8::is_ascii_digit)
		&& v.chars().all(|c| {
			c.is_ascii_digit() || matches!(c, '-' | ':' | 't' | 'T' | 'z' | 'Z' | '.' | '+' | ' ')
		})
}

/// Match a Go-style duration: one or more `<number><unit>` segments, units one
/// of `ns,us,µs,ms,s,m,h`.
fn is_go_duration(v: &str) -> bool {
	let mut rest = v.strip_prefix('-').unwrap_or(v);
	if rest.is_empty() {
		return false;
	}
	let mut segments = 0;
	while !rest.is_empty() {
		let digits = rest.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
		if digits.len() == rest.len() {
			// No digits consumed → not a duration segment.
			return false;
		}
		rest = digits;
		let unit_len = ["ms", "ns", "us", "µs", "s", "m", "h"]
			.into_iter()
			.find(|u| rest.starts_with(u))
			.map(str::len);
		match unit_len {
			Some(n) => rest = &rest[n..],
			None => return false,
		}
		segments += 1;
	}
	segments > 0
}

/// The label `logs` tags a container's lines with: the container name minus the
/// project prefix, so `myproj-web-1` reads as `web-1`.
///
/// Attached `up` already strips it this way (`inspect.rs`), so before this the
/// same container was tagged `myproj-web-1  | ` by one command and `web-1 | ` by
/// the other — two shapes for one thing, in one binary. docker compose prints
/// the short form.
pub(crate) fn display_label(container_name: &str, project: &str) -> String {
	container_name
		.strip_prefix(&format!("{project}-"))
		.unwrap_or(container_name)
		.to_string()
}

/// Whether a failed write to the log sink should end the follow loop.
///
/// A `BrokenPipe` is the ordinary way a piped consumer signals it has read
/// enough — `logs -f | head`, `| grep -q`, `| less` and quit. It is a clean end
/// of output, not a failure, and the loop must stop: podup used to discard the
/// write result entirely and go on streaming into a dead pipe until the process
/// was killed. Any other io error is worth a warning before stopping, since it
/// means output is being lost for a reason the user cannot see.
fn stop_on_write_error(container_name: &str, result: std::io::Result<()>) -> bool {
	match result {
		Ok(()) => false,
		Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => true,
		Err(e) => {
			tracing::warn!("logs {container_name}: cannot write output: {e}");
			true
		}
	}
}

/// Whether a log stream that ended with an error truncated live output.
///
/// `still_running` is the out-of-band re-check: `Some(true)` the container is
/// still up, `Some(false)` it has stopped or is gone, `None` the state could not
/// be read.
///
/// `None` counts as a break. It is tempting to read "could not tell" as a clean
/// end, and that is wrong in the exact case this matters: the severed connection
/// that ends the stream is usually the same one the re-check needs, so treating
/// the unknown as success turns every transport failure back into exit 0 — which
/// is the bug, not the fix. Measured: with the permissive version, restarting the
/// libpod socket under an attached `logs -f` reported a still-running container
/// as stopped.
///
/// Pure so the rule is pinned without a live socket.
pub(super) fn stream_broke_mid_output(still_running: Option<bool>) -> bool {
	!matches!(still_running, Some(false))
}

/// Build the libpod `containers/{}/logs` query string from the options.
fn log_query(opts: &LogsOptions) -> String {
	let mut q = format!(
		"stdout=true&stderr=true&follow={}&timestamps={}",
		opts.follow, opts.timestamps
	);
	if let Some(tail) = &opts.tail {
		q.push_str(&format!("&tail={}", urlencoded(tail)));
	}
	if let Some(since) = &opts.since {
		q.push_str(&format!("&since={}", urlencoded(since)));
	}
	if let Some(until) = &opts.until {
		q.push_str(&format!("&until={}", urlencoded(until)));
	}
	q
}

impl Engine {
	/// Stream logs. When `service_name` is `None`, streams from all services. When `follow` is true, tails indefinitely.
	pub async fn logs(
		&self,
		file: &ComposeFile,
		service_name: Option<&str>,
		follow: bool,
	) -> Result<()> {
		let targets: Vec<String> = service_name
			.map(|s| vec![s.to_string()])
			.unwrap_or_default();
		self.logs_with_options(
			file,
			&targets,
			LogsOptions {
				follow,
				..Default::default()
			},
		)
		.await
	}

	/// Stream logs with `docker compose logs` options (`--tail`, `--since`,
	/// `--until`, `--timestamps`, `--follow`). For the `--no-color`/
	/// `--no-log-prefix` prefix-display options use [`Engine::logs_with_display`].
	///
	/// When `target_services` is empty, logs from every service are streamed;
	/// otherwise only the named services (an unknown name is an error).
	pub async fn logs_with_options(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		opts: LogsOptions,
	) -> Result<()> {
		self.logs_with_display(file, target_services, opts, LogsDisplay::default())
			.await
	}

	/// Stream logs with `docker compose logs` options plus the prefix-display
	/// controls (`--no-color`, `--no-log-prefix`).
	///
	/// When `target_services` is empty, logs from every service are streamed;
	/// otherwise only the named services (an unknown name is an error).
	pub async fn logs_with_display(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		opts: LogsOptions,
		display: LogsDisplay,
	) -> Result<()> {
		validate_log_filters(&opts)?;
		let follow = opts.follow;
		// `--no-log-prefix` drops the `{service} | ` tag; `--no-color` forces a
		// monochrome prefix even on a colour-capable stdout.
		let prefix = !display.no_log_prefix;
		let allow_color = !display.no_color;
		let query = log_query(&opts);
		for svc in target_services {
			if !file.services.contains_key(svc) {
				return Err(ComposeError::ServiceNotFound(svc.into()));
			}
		}
		let selected: std::collections::HashSet<&str> =
			target_services.iter().map(String::as_str).collect();
		// (container_name, is_tty) — TTY containers send raw bytes; non-TTY use
		// multiplexed 8-byte-header framing. Resolved against the containers
		// Podman actually has (`live_replica_names`), not the static compose
		// replica count: after a runtime `scale`/`up --scale` the file's count no
		// longer matches the live replicas, so `logs` would otherwise miss every
		// replica beyond the first (falls back to the static names when none are
		// running yet).
		// One `live_replica_names` round-trip per selected service (a future
		// optimization: batch this through scale.rs's `live_project_replicas`
		// instead). A resolution failure for
		// one service must not blank the whole command the way an `.await?` would:
		// warn and skip that service so the rest still stream, matching the
		// per-container tolerance below.
		// A failure resolving ONE service is tolerated so the others still print —
		// that is deliberate and tested. But when nothing resolves at all, the
		// command printed nothing and still reported success, which is how an
		// unreachable engine looked identical to a project with no logs. Kept and
		// only consulted when there is no output to salvage.
		let mut first_err: Option<ComposeError> = None;
		let mut targets: Vec<(String, bool)> = Vec::new();
		for (n, s) in file
			.services
			.iter()
			.filter(|(n, _)| selected.is_empty() || selected.contains(n.as_str()))
		{
			let is_tty = s.tty.unwrap_or(false);
			let names = match self.live_replica_names(n, s).await {
				Ok(names) => names,
				Err(e) => {
					tracing::warn!("logs: resolving replicas for service {n}: {e}");
					first_err.get_or_insert(e);
					continue;
				}
			};
			for cname in names {
				targets.push((cname, is_tty));
			}
		}

		// A container that is simply not there yet is tolerated per-container so
		// the services that *do* exist still stream — that is deliberate. Anything
		// else (the socket refusing, a 500) is not a per-container fact, it is the
		// command failing, and `logs` reported exit 0 for it. Collected here and
		// returned at the end so every reachable container is still shown first.
		//
		// Nothing resolved and something went wrong: there is no partial result to
		// preserve, so the tolerance has nothing left to protect. This is separate
		// from #1104 — nothing here classifies how a stream *ended*, the request
		// never opened.
		if targets.is_empty() {
			if let Some(e) = first_err {
				return Err(e);
			}
		}

		// Same rule one level down: a container that will not stream is tolerated
		// while another does, but every target failing is the command failing.
		let mut streamed_err: Option<ComposeError> = None;
		// A stream that truncated live output is NOT the same class as a container
		// that would not open one. The latter is tolerated while another streams;
		// the former means output was lost, so it must survive the `streamed_any`
		// shortcut below rather than being masked by a sibling that streamed fine
		// (#1104).
		let mut truncated_err: Option<ComposeError> = None;
		let mut streamed_any = false;
		let target_count = targets.len();

		// When follow=true, streams never end until containers stop. Run them
		// concurrently so multiple containers don't block each other.
		if follow && targets.len() > 1 {
			let futs: Vec<_> = targets
				.into_iter()
				.map(|(container_name, is_tty)| {
					let client = &self.client;
					let query = query.clone();
					async move {
						let path = format!(
							"{API_PREFIX}/containers/{}/logs?{query}",
							urlencoded(&container_name),
						);
						let resp = match client.get_stream(&path).await {
							Ok(r) => r,
							Err(e) => {
								tracing::warn!("logs {container_name}: {e}");
								return Some(e);
							}
						};
						let mut stream = if is_tty {
							crate::libpod::parse_raw(resp.into_body())
						} else {
							crate::libpod::parse_multiplexed(resp.into_body())
						};
						// These futures run concurrently under `join_all` on the
						// same task, so the stdout/stderr lock is taken and
						// released within each frame rather than held across the
						// `.await` above — holding a guard across the await would
						// let a sibling future block the thread on the same lock
						// and deadlock. Each frame still locks once and flushes,
						// keeping interleaved `logs -f` output prompt.
						let label = display_label(&container_name, &self.project);
						let mut out_pfx = LinePrefixer::new(&label, prefix, allow_color);
						let mut err_pfx = LinePrefixer::new(&label, prefix, allow_color);
						while let Some(msg) = stream.next().await {
							let wrote = match msg {
								Ok(LogOutput::StdOut { message }) => {
									out_pfx.write(&mut std::io::stdout().lock(), &message)
								}
								Ok(LogOutput::StdErr { message }) => {
									err_pfx.write(&mut std::io::stderr().lock(), &message)
								}
								// A `logs -f` stream ends when its container stops, and
								// libpod marks that with a chunked terminator. A lost
								// terminator arrives here as an `Err` indistinguishable
								// at the transport layer from a real mid-stream break
								// (#1104), so resolve it out of band: if the container is
								// still running, live output was truncated and the
								// command failed.
								//
								// Measured on real 5.4.2 by restarting the libpod socket
								// under an attached `logs -f`: the arm is reached, the
								// container is still running, and this used to exit 0.
								// docker compose reports `unexpected EOF` and exits 1 on
								// the same failure, so 0 was a divergence, not parity.
								//
								// The re-check is point-in-time, so a genuine break that
								// coincides with the container stopping is knowingly
								// read as a clean end — the transport cannot separate
								// them and the container is gone either way.
								Err(e) => {
									let kind = e.stream_end_kind();
									match stream_broke_mid_output(
										self.container_still_running(&container_name).await,
									) {
										true => {
											tracing::warn!(
												"logs {container_name}: stream ended while the \
												 container was still running [{kind}]: {e}"
											);
											return Some(e);
										}
										false => {
											tracing::warn!(
												"logs {container_name}: stream ended as the \
												 container stopped [{kind}]"
											);
										}
									}
									break;
								}
							};
							if stop_on_write_error(&container_name, wrote) {
								break;
							}
						}
						out_pfx.flush_tail(&mut std::io::stdout().lock());
						err_pfx.flush_tail(&mut std::io::stderr().lock());
						None
					}
				})
				.collect();
			let mut failures = 0usize;
			for e in futures_util::future::join_all(futs)
				.await
				.into_iter()
				.flatten()
			{
				failures += 1;
				streamed_err.get_or_insert(ComposeError::Podman(e));
			}
			streamed_any = failures < target_count;
		} else {
			for (container_name, is_tty) in targets {
				let path = format!(
					"{API_PREFIX}/containers/{}/logs?{query}",
					urlencoded(&container_name),
				);
				// Tolerate a missing/not-yet-created container the way the
				// multi-follow path does: warn and move on so the logs of the
				// services that *do* exist are still shown, instead of aborting the
				// whole command on the first 404.
				let resp = match self.client.get_stream(&path).await {
					Ok(r) => r,
					Err(e) => {
						tracing::warn!("logs {container_name}: {e}");
						streamed_err.get_or_insert(ComposeError::Podman(e));
						continue;
					}
				};
				let mut stream = if is_tty {
					crate::libpod::parse_raw(resp.into_body())
				} else {
					crate::libpod::parse_multiplexed(resp.into_body())
				};

				// Lock stdout once for the whole stream instead of re-acquiring
				// the lock (and issuing a syscall) per frame; stdout is ours
				// exclusively on this path. stderr is locked per frame because
				// the tracing subscriber also writes there: holding its lock
				// across the await loop would starve concurrent log emissions.
				// Flush after each frame so `logs -f` still streams promptly.
				let mut out = std::io::stdout().lock();
				let label = display_label(&container_name, &self.project);
				let mut out_pfx = LinePrefixer::new(&label, prefix, allow_color);
				let mut err_pfx = LinePrefixer::new(&label, prefix, allow_color);
				while let Some(msg) = stream.next().await {
					let wrote = match msg {
						Ok(LogOutput::StdOut { message }) => out_pfx.write(&mut out, &message),
						Ok(LogOutput::StdErr { message }) => {
							err_pfx.write(&mut std::io::stderr().lock(), &message)
						}
						// Same out-of-band resolution as the concurrent path above: a
						// stream that ends while its container is still running
						// truncated live output (#1104).
						Err(e) => {
							let kind = e.stream_end_kind();
							match stream_broke_mid_output(
								self.container_still_running(&container_name).await,
							) {
								true => {
									tracing::warn!(
										"logs {container_name}: stream ended while the container \
										 was still running [{kind}]: {e}"
									);
									truncated_err.get_or_insert(ComposeError::Podman(e));
								}
								false => {
									tracing::warn!(
										"logs {container_name}: stream ended as the container \
										 stopped [{kind}]"
									);
								}
							}
							break;
						}
					};
					if stop_on_write_error(&container_name, wrote) {
						break;
					}
				}
				out_pfx.flush_tail(&mut out);
				err_pfx.flush_tail(&mut std::io::stderr().lock());
				streamed_any = true;
			}
		}

		// Checked before the tolerance shortcut: output was lost, and another
		// container streaming cleanly does not make that untrue.
		if let Some(e) = truncated_err {
			return Err(e);
		}
		if streamed_any {
			return Ok(());
		}
		streamed_err.map_or(Ok(()), Err)
	}

	/// Whether `container_name` is still running, for resolving how a log stream
	/// ended.
	///
	/// A `logs -f` stream lives as long as its container runs and ends when the
	/// container stops. libpod signals that end with a chunked terminator, and a
	/// lost terminator — a dropped connection, or a version that omits it —
	/// arrives as an `Err` that the transport layer cannot tell apart from a real
	/// mid-stream break (#1104). Re-checking the container out of band resolves
	/// it: still running means the stream truncated live output.
	///
	/// `Ok(None)` is "could not tell" — an unreadable state is not confirmation
	/// the end was expected, so the caller keeps the original error rather than
	/// masking a possible failure. Mirrors the fail-closed rule `stats` uses.
	pub(super) async fn container_still_running(&self, container_name: &str) -> Option<bool> {
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			self.project_label_filter_encoded(),
		);
		let entries = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.ok()?;
		entries
			.iter()
			.find(|e| {
				e.names
					.iter()
					.any(|raw| raw.trim_start_matches('/') == container_name)
			})
			.map(|e| e.state == "running")
			// A container the listing no longer holds has been removed, which is a
			// stop by any other name — the stream had every reason to end.
			.or(Some(false))
	}

	/// Names of this project's containers (by label) that the current compose file
	/// no longer defines — the orphans, shared by removal and the warning.
	async fn orphan_container_names(&self, file: &ComposeFile) -> Result<Vec<String>> {
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			self.project_label_filter_encoded(),
		);

		let running = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;

		let known: std::collections::HashSet<String> = file
			.services
			.iter()
			.flat_map(|(n, s)| self.replica_names(n, s))
			.collect();

		let names: Vec<String> = running
			.iter()
			.flat_map(|c| c.names.iter())
			.map(|raw| raw.trim_start_matches('/').to_string())
			.collect();
		Ok(filter_orphans(names, &known))
	}

	/// Remove containers labelled for this project that are not defined in the current compose file.
	///
	/// Best-effort across every orphan — one that fails to remove must not stop
	/// the rest from being reaped — but the first real failure is remembered and
	/// returned once every orphan has been attempted, so a removal that
	/// genuinely fails does not exit 0 with the orphan left behind (#598). A 404
	/// (already gone) stays an idempotent no-op.
	pub async fn remove_orphans(&self, file: &ComposeFile) -> Result<()> {
		let mut first_err: Option<ComposeError> = None;
		for name in self.orphan_container_names(file).await? {
			tracing::info!("removing orphan container {name}");
			let rm_path = format!("{API_PREFIX}/containers/{}?force=true", urlencoded(&name));
			match self.client.delete_ok(&rm_path).await {
				Ok(()) => {}
				Err(e) if e.is_status(404) => {}
				Err(e) => {
					tracing::debug!("orphan delete {name}: {e}");
					first_err.get_or_insert(ComposeError::Podman(e));
				}
			}
		}
		if let Some(e) = first_err {
			return Err(e);
		}
		Ok(())
	}

	/// Warn (without removing) when this project has orphan containers and
	/// `--remove-orphans` was not given, matching docker compose's `up`.
	pub async fn warn_orphans(&self, file: &ComposeFile) -> Result<()> {
		let orphans = self.orphan_container_names(file).await?;
		if !orphans.is_empty() {
			// Through tracing like every other warning: this printed with no
			// `podup:` prefix and no `warning` label at all, so the one message
			// telling a user their compose file drifted read as stray output.
			tracing::warn!(
				"found orphan container(s) ({}) for this project. If you removed or renamed a \
				 service in your compose file, run with --remove-orphans to remove them.",
				orphans.join(", ")
			);
		}
		Ok(())
	}
}

/// The subset of `names` not present in `known` (the orphan containers). Pure so
/// the membership logic is unit-tested without a live Podman socket.
fn filter_orphans(names: Vec<String>, known: &std::collections::HashSet<String>) -> Vec<String> {
	names.into_iter().filter(|n| !known.contains(n)).collect()
}

/// Kept out of `inspect.rs`, which is 11 lines under the 500-line limit.
#[cfg(test)]
mod attach_stream_tests;
#[cfg(test)]
mod tests;
