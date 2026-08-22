//! Log-attach subsystem for `up`: the `attach_logs*` family and the abort path.
//!
//! The single-service `attach` / `attach_with_index` commands and the
//! `attach_log_query` helper that backs them stay in `inspect.rs`; this file
//! holds everything that follows the multi-service streams — the public
//! `AttachOutcome` / `AttachOptions` / `AttachSummary` types, the abort
//! plumbing (`Phase`, `StreamEnd`, `wait_for_phase`, `container_exit_code`)
//! and the `Engine::attach_logs*` methods that produce them. Split out of
//! `inspect.rs` so each file fits under the 500-line hard limit.

use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, LogOutput, API_PREFIX};

use super::Engine;

/// How an attached `up` stopped streaming.
///
/// The distinction has to survive back to the caller because the four endings
/// mean different things to a script: the containers finishing on their own is
/// success, the operator pressing Ctrl-C is not, a stream that died under a
/// container still running is a failed read, and an abort triggered by a
/// container exit carries the exit code the caller wants to propagate. The
/// caller still tears the project down in every case except the abort, where
/// `attach` itself already stopped every remaining container. Reporting an
/// ending as an error from `attach` would short-circuit that and leave the
/// containers running, which is a worse bug than the exit code this exists to
/// fix.
///
/// When the outcome is `Aborted`, the service name and exit code travel back
/// alongside it in [`AttachSummary`], not as fields of the variant: keeping the
/// enum all-unit preserves the `Copy` derive this enum had before
/// `--abort-on-container-exit` (#1492) and keeps the defined discriminants
/// every `as isize`/`mem::discriminant` caller relies on. The variant name was
/// the only piece of the struct that survived the rework; everything that
/// used to live inside the braces moved one level up.
///
/// `#[non_exhaustive]` since 3.0.0, so a further ending can be added without a
/// major bump. `StreamBroke` (3.3.0) and `Aborted` (#1492) are two that already
/// were.
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
	/// `up --abort-on-container-exit` (or `--exit-code-from`, which implies it)
	/// fired: the first container to exit triggered the abort, the rest were
	/// stopped, and the exit code is what the CLI propagates as its process
	/// exit status. The service name and exit code are carried in the
	/// sibling [`AttachSummary::service`] / [`AttachSummary::exit_code`]
	/// fields, not as struct fields here, so the enum stays all-unit and the
	/// `Copy` derive survives.
	Aborted,
}

/// Options that go with [`Engine::attach_logs_with`] — every flag `podup up`
/// exposes that affects what an attached stream does or how it ends.
///
/// `#[non_exhaustive]` since 4.1.0, so the next flag is not a breaking change
/// for anyone building one with a literal. [`AttachOptions::default`] builds
/// the no-op set, and the rest of the surface is the `with_*` builders below.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AttachOptions {
	/// Prefix each streamed line with the libpod RFC3339 timestamp
	/// (`up --timestamps`).
	pub timestamps: bool,
	/// Stop the rest of the project as soon as any container exits
	/// (`up --abort-on-container-exit`). See [`AttachOutcome::Aborted`] for
	/// what the caller is then expected to do.
	pub abort_on_container_exit: bool,
	/// When set, the exit code reported back is the named service's code
	/// rather than the first container's. Implies `abort_on_container_exit`
	/// (matching `docker compose` v5.1.3): the abort has to fire to learn a
	/// later exit code, so passing only `--exit-code-from` is enough.
	pub exit_code_from: Option<String>,
}

impl AttachOptions {
	/// The no-options set: every flag off, `--timestamps` off, no named exit
	/// source. Equivalent to [`AttachOptions::default`].
	#[must_use]
	pub fn new() -> Self {
		Self::default()
	}

	/// Set [`AttachOptions::timestamps`]. Builder-style.
	#[must_use]
	pub fn with_timestamps(mut self, timestamps: bool) -> Self {
		self.timestamps = timestamps;
		self
	}

	/// Set [`AttachOptions::abort_on_container_exit`]. Builder-style.
	#[must_use]
	pub fn with_abort_on_container_exit(mut self, abort_on_container_exit: bool) -> Self {
		self.abort_on_container_exit = abort_on_container_exit;
		self
	}

	/// Set [`AttachOptions::exit_code_from`]. Builder-style. The named
	/// service is checked against the compose file in
	/// [`Engine::attach_logs_with`], not here, so the validation error
	/// surfaces as `ServiceNotFound` from the call itself.
	#[must_use]
	pub fn with_exit_code_from(mut self, exit_code_from: Option<String>) -> Self {
		self.exit_code_from = exit_code_from;
		self
	}
}

/// What an attached `up` returned, pairing the always-known [`AttachOutcome`]
/// with the abort-specific extras that previously lived as fields of the
/// `Aborted` variant.
///
/// `service` and `exit_code` are only meaningful when `outcome` is
/// [`AttachOutcome::Aborted`]; they are `None` for the other three endings.
/// The two-abort-fields split off into this struct is what keeps the enum
/// `Copy` (and keeps its discriminant values defined) — putting a `String`
/// next to an `i64` inside the variant would have broken both, and the
/// caller still needs to read them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachSummary {
	/// Which of the four endings the attach call reached.
	pub outcome: AttachOutcome,
	/// The compose service whose exit code is being propagated, on the
	/// `Aborted` path. `None` for every other ending.
	pub service: Option<String>,
	/// The exit code reported by `service`'s container, after the abort
	/// stopped everything. May be `0` (a clean exit), non-zero (a crash),
	/// or `137` (SIGKILL during the abort teardown when the named service
	/// was still running). `None` for every non-`Aborted` ending.
	pub exit_code: Option<i64>,
}

impl Engine {
	/// Attach to log streams for all services with `attach: true` (the default). Streams are multiplexed to stdout with a service-name prefix.
	pub async fn attach_logs(&self, file: &ComposeFile) -> Result<AttachOutcome> {
		self.attach_logs_with_options(file, false).await
	}

	/// Like [`Engine::attach_logs`] but with `up --timestamps` support.
	///
	/// `timestamps` prefixes each streamed line with the libpod RFC3339
	/// timestamp. This is the legacy two-parameter form kept verbatim for
	/// 4.0.0 callers; the abort-related flags moved to
	/// [`Engine::attach_logs_with`] so they could be added without breaking
	/// this signature. A caller that does not need the abort path can keep
	/// using this method and see no behaviour change between 4.0.0 and
	/// 4.1.0.
	pub async fn attach_logs_with_options(
		&self,
		file: &ComposeFile,
		timestamps: bool,
	) -> Result<AttachOutcome> {
		// Delegate with the abort set fully off, so the call path the rest of
		// the 4.0.0 callers still take is exercised by the wrapper itself —
		// not just by the underlying method when the new flags are turned on.
		self.attach_logs_with(
			file,
			&AttachOptions {
				timestamps,
				abort_on_container_exit: false,
				exit_code_from: None,
			},
		)
		.await
		.map(|summary| summary.outcome)
	}

	/// Like [`Engine::attach_logs`] but with `up --timestamps` and
	/// `--abort-on-container-exit` / `--exit-code-from` support.
	///
	/// `options.timestamps` prefixes each streamed line with the libpod
	/// RFC3339 timestamp.
	///
	/// `options.abort_on_container_exit` (and `options.exit_code_from`, which
	/// implies it) makes the call return
	/// [`AttachSummary::outcome`] = [`AttachOutcome::Aborted`] as soon as any
	/// container exits, with the propagating service name and exit code in
	/// the sibling fields of the returned [`AttachSummary`]. On that path the
	/// remaining containers are stopped before the function returns, so the
	/// caller does not need to call [`Engine::stop`] — and `dispatch.rs` skips
	/// its own stop call on that outcome for the same reason.
	///
	/// The two abort-specific fields live on [`AttachSummary`] rather than as
	/// struct fields of [`AttachOutcome::Aborted`]: keeping the enum all-unit
	/// preserves the `Copy` derive it had in 4.0.0 and keeps the defined
	/// discriminants every `as isize` / `mem::discriminant` caller relies on.
	/// Callers that just need to know which ending happened can still
	/// pattern-match `summary.outcome` exactly as they did on 4.0.0; the
	/// sibling fields are only meaningful when the outcome is `Aborted`.
	///
	/// A service name passed as `options.exit_code_from` must exist in the
	/// compose file; a missing service is rejected with
	/// [`ComposeError::ServiceNotFound`] before any work happens (matching
	/// `docker compose` v5.1.3).
	pub async fn attach_logs_with(
		&self,
		file: &ComposeFile,
		options: &AttachOptions,
	) -> Result<AttachSummary> {
		// Reject `--exit-code-from` naming an unknown service up front. Doing this
		// before any container is created means a typo surfaces as a clear
		// "service X not found" error, matching docker compose v5.1.3.
		if let Some(target) = options.exit_code_from.as_deref() {
			if !file.services.contains_key(target) {
				return Err(ComposeError::ServiceNotFound(target.to_string()));
			}
		}

		let timestamps = options.timestamps;
		let abort_on_container_exit =
			options.abort_on_container_exit || options.exit_code_from.is_some();

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
			return Ok(AttachSummary {
				outcome: AttachOutcome::StreamsEnded,
				service: None,
				exit_code: None,
			});
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
			Phase::Interrupted => Ok(AttachSummary {
				outcome: AttachOutcome::Interrupted,
				service: None,
				exit_code: None,
			}),
			Phase::AllEnded { saw_break } => {
				let outcome = if saw_break {
					AttachOutcome::StreamBroke
				} else {
					AttachOutcome::StreamsEnded
				};
				Ok(AttachSummary {
					outcome,
					service: None,
					exit_code: None,
				})
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
				let (service, exit_code) = match options.exit_code_from.as_deref() {
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

				Ok(AttachSummary {
					outcome: AttachOutcome::Aborted,
					service: Some(service),
					exit_code: Some(exit_code),
				})
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
