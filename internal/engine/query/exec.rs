//! `exec` command: run a command inside a service container.

use std::io::Write;

use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::types::exec::{
	ExecCreateConfig, ExecCreateResponse, ExecInspect, ExecStartConfig,
};
use crate::libpod::{urlencoded, LogOutput, API_PREFIX};

use super::Engine;

/// Ceiling on how long to wait for the libpod exec-start *response head*. A
/// healthy engine returns it almost instantly: either the hijacked stream or a
/// prompt error (e.g. HTTP 500 "unable to find user … no matching entries in
/// passwd file"). When the target user/workdir does not resolve, some engine
/// builds instead stall before answering, which the default client read timeout
/// would only abort after ~120s and then report as a misleading socket-timeout.
/// Bounding the head here lets [`Engine::exec_with_options`] fail fast with a
/// clear, exec-specific message. It covers only the head; the streamed exec
/// output is left unbounded so a legitimate long-running command runs to
/// completion.
const EXEC_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Options for [`Engine::exec`], mirroring `docker compose exec` flags.
///
/// `#[non_exhaustive]`: built with `..Default::default()`, so adding a flag as
/// the exec surface grows is not another breaking change. The two autostart
/// option structs learned the same lesson in the same release.
#[derive(Default)]
#[non_exhaustive]
pub struct ExecOptions {
	/// Extra environment variables (`KEY=VAL`), `-e/--env`.
	pub env: Vec<String>,
	/// Run as this user, `-u/--user`.
	pub user: Option<String>,
	/// Working directory inside the container, `-w/--workdir`.
	pub workdir: Option<String>,
	/// Run with extended privileges, `--privileged`.
	pub privileged: bool,
	/// Detach: start the exec and return without streaming output, `-d/--detach`.
	pub detach: bool,
	/// 1-based replica index for a scaled service, `--index` (default: first).
	pub index: Option<u32>,
	/// Suppress the pseudo-terminal, `-T/--no-tty`.
	///
	/// Interactive is the default when stdin is a terminal, matching
	/// `docker compose exec`; this is how a caller opts out. It is also what a
	/// non-terminal stdin implies, so a scripted `exec` behaves the same with or
	/// without the flag.
	pub no_tty: bool,
}

impl ExecOptions {
	/// Run the command as this user, `-u/--user`. Builder-style.
	pub fn with_user(mut self, user: Option<String>) -> Self {
		self.user = user;
		self
	}

	/// Working directory inside the container, `-w/--workdir`. Builder-style.
	pub fn with_workdir(mut self, workdir: Option<String>) -> Self {
		self.workdir = workdir;
		self
	}

	#[cfg(test)]
	fn with_no_tty_for_test(mut self, v: bool) -> Self {
		self.no_tty = v;
		self
	}

	#[cfg(test)]
	fn with_detach_for_test(mut self, v: bool) -> Self {
		self.detach = v;
		self
	}

	/// Target this replica (1-based) of a scaled service, `--index`.
	/// Builder-style.
	pub fn with_index(mut self, index: Option<u32>) -> Self {
		self.index = index;
		self
	}

	/// Extra environment variables (`KEY=VAL`), `-e/--env`. Builder-style.
	pub fn with_env(mut self, env: Vec<String>) -> Self {
		self.env = env;
		self
	}

	/// Every `docker compose exec` flag, in CLI order. A constructor rather than
	/// a struct literal because the type is `#[non_exhaustive]`, so the next flag
	/// to land is not a breaking change for anyone building one.
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		env: Vec<String>,
		user: Option<String>,
		workdir: Option<String>,
		privileged: bool,
		detach: bool,
		index: Option<u32>,
		no_tty: bool,
	) -> Self {
		Self {
			env,
			user,
			workdir,
			privileged,
			detach,
			index,
			no_tty,
		}
	}
}

/// Build the exec environment list, expanding a bare `KEY` (no `=`) from podup's
/// own environment the way docker-compose does. A `KEY=VALUE` entry passes
/// through unchanged; a value-less `KEY` is replaced with `KEY=<host value>` and
/// dropped entirely when the variable is unset (libpod rejects a bare key with
/// HTTP 400). Pure so it is unit-tested without a container.
fn expand_exec_env(env: &[String]) -> Vec<String> {
	env.iter()
		.filter_map(|e| {
			if e.contains('=') {
				Some(e.clone())
			} else {
				std::env::var(e).ok().map(|v| format!("{e}={v}"))
			}
		})
		.collect()
}

/// True for the in-band stream-teardown line Podman/conmon emits on the exec
/// stderr channel when an exec launch fails (e.g. a bad `--workdir`/`--user`):
/// a secondary `read unixpacket ... connection reset by peer` frame that adds
/// nothing to the real diagnostic. Matching is deliberately narrow so ordinary
/// program output is never suppressed: a bare `contains` on the two substrings
/// would also suppress a Go program that logs `dial unixpacket: connection reset
/// by peer` (#1369). The match anchors on the system-noise prefix the standard
/// library prints for that error, which a real program's log does not carry.
fn is_exec_teardown_noise(line: &str) -> bool {
	// The two shapes Rust/glibc and Go print, kept as a small set so a real
	// program's log never collides. The system prefix (`os error 104` on
	// Linux, `Connection reset by peer` from a lower-level `cargo`/`conmon`
	// log) is what narrows the match: a real Go `dial unixpacket: ...` line
	// does not start with `read unixpacket`.
	line.contains("read unixpacket") && line.contains("connection reset by peer")
}

/// Map a libpod error from an `exec` target into a friendly
/// [`ComposeError::NotRunning`] when it means the container is absent (404) or
/// stopped ("can only create exec sessions on running containers"), so the user
/// sees "service X is not running" instead of a raw HTTP 404/500. Any other
/// failure passes through unchanged. Pure so it is unit-tested.
fn map_not_running(e: crate::libpod::PodmanError, service_name: &str) -> ComposeError {
	let not_running = e.is_status(404)
		|| matches!(
			&e,
			crate::libpod::PodmanError::Api { message, .. }
				if {
					let m = message.to_ascii_lowercase();
					m.contains("can only create exec sessions on running containers")
						|| m.contains("is not running")
						|| m.contains("no such container")
				}
		);
	if not_running {
		ComposeError::NotRunning(service_name.to_string())
	} else {
		ComposeError::Podman(e)
	}
}

/// Translate a failure *starting* the exec session into a clear error. A
/// client-side timeout means the libpod exec-start never returned its response
/// head within [`EXEC_START_TIMEOUT`], almost always a wedged launch (e.g. a
/// nonexistent `--user`/`--workdir` the server stalls resolving) rather than an
/// unhealthy socket, so surface that with the likely cause instead of the
/// generic "timed out waiting for the Podman socket" message. Every other error,
/// including the prompt HTTP error an engine *does* return for a bad user
/// ("unable to find user … no matching entries in passwd file"), passes through
/// unchanged so legitimate diagnostics are never masked. Pure so it is
/// unit-tested.
fn map_exec_start_err(e: crate::libpod::PodmanError, opts: &ExecOptions) -> ComposeError {
	if e.is_timeout() {
		let cause = if let Some(user) = &opts.user {
			format!(" (the requested user '{user}' may not exist in the container)")
		} else if let Some(dir) = &opts.workdir {
			format!(" (the requested working directory '{dir}' may not exist)")
		} else {
			String::new()
		};
		return ComposeError::ExecFailed(format!(
			"the exec session did not start within {}s{cause}",
			EXEC_START_TIMEOUT.as_secs()
		));
	}
	ComposeError::Podman(e)
}

impl Engine {
	/// Run a command in the first replica of the named service with default
	/// options. Exits with the command's exit code.
	pub async fn exec(
		&self,
		file: &ComposeFile,
		service_name: &str,
		cmd: Vec<String>,
	) -> Result<()> {
		self.exec_with_options(file, service_name, cmd, ExecOptions::default())
			.await
	}

	/// Run a command in a service container with `docker compose exec`-style
	/// overrides (env, user, workdir, privileged, detach, replica index).
	pub async fn exec_with_options(
		&self,
		file: &ComposeFile,
		service_name: &str,
		cmd: Vec<String>,
		opts: ExecOptions,
	) -> Result<()> {
		// An empty command would be forwarded as an empty `cmd` and surface a raw
		// podman HTTP 500; reject it up front with a clear message.
		if cmd.is_empty() {
			return Err(ComposeError::Unsupported(
				"exec: a command is required".into(),
			));
		}
		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into()))?;
		// Surface the same host-binding / privilege-escalation warnings the
		// `up` path would have emitted, so `exec` against a privileged or
		// host-networked container does not appear safer than the same
		// container's `up`. The container itself was created with these modes,
		// but `exec` is the operator's chance to notice them, and the command
		// they are about to run executes inside that container. `--no-warn`
		// still opts out.
		if !self.no_warn {
			for w in crate::engine::container::check_host_mode(service_name, service) {
				tracing::warn!("{}", w.message);
			}
		}
		// Resolve the target replica against the *running* containers (matching
		// `cp`), so `--index N` and a bare `exec` reach a live replica of a service
		// scaled by an earlier `up --scale`/`scale`, not just the compose static
		// count. `--index 0`/out-of-range indexes stay rejected consistently.
		let container_name = self
			.live_replica_name_at(service_name, service, opts.index)
			.await?;

		// Interactive when there is a terminal to be interactive with, unless the
		// caller opted out or detached. This is `docker compose exec`'s rule: no
		// `-i` flag, because a TTY on both ends is the default and `-T` is how you
		// say no. A non-terminal stdin (a script, a pipeline) takes the
		// unchanged streaming path, so nothing about scripted `exec` moves.
		let interactive = interactive_exec(&opts);

		let env = expand_exec_env(&opts.env);
		let exec_cfg = ExecCreateConfig {
			cmd: Some(cmd),
			attach_stdout: Some(true),
			attach_stderr: Some(true),
			attach_stdin: interactive.then_some(true),
			tty: interactive.then_some(true),
			user: opts.user.clone(),
			working_dir: opts.workdir.clone(),
			privileged: opts.privileged.then_some(true),
			env: (!env.is_empty()).then_some(env),
		};
		let create_path = format!(
			"{API_PREFIX}/containers/{}/exec",
			urlencoded(&container_name),
		);
		let resp: ExecCreateResponse = match self.client.post_json(&create_path, &exec_cfg).await {
			Ok(resp) => resp,
			// The exec create is where the drops measured in #1339 land most
			// often: twice of six, more than any other endpoint. Unlike the
			// lifecycle calls, a lost response here cannot be resolved by looking
			// at the container: what is lost is the exec id, and a container
			// running several execs at once cannot say which of its `ExecIDs` was
			// this one.
			//
			// Retrying is safe instead, because creating an exec changes nothing.
			// Measured on Podman 5.7.0: five execs created and never started leave
			// the container `running` on the same pid with no extra process
			// inside; they are inert handles that die with it. So the worst case
			// of a retry is one leaked handle, against an `exec` that fails for a
			// reason that was never about the command.
			//
			// Once only: a second drop is a daemon problem, not a transient.
			Err(e) if e.is_incomplete_message() => {
				tracing::warn!(
					"{container_name}: the exec create lost its response [{}]; retrying once",
					e.stream_end_kind()
				);
				self.client
					.post_json(&create_path, &exec_cfg)
					.await
					.map_err(|e| map_not_running(e, service_name))?
			}
			Err(e) => return Err(map_not_running(e, service_name)),
		};
		let exec_id = resp.id;

		// `-d/--detach`: start the exec and return without streaming output or
		// waiting for the exit code. The server returns immediately, so the
		// response body is dropped.
		if opts.detach {
			let start_cfg = ExecStartConfig {
				detach: true,
				tty: false,
			};
			let start_path = format!("{API_PREFIX}/exec/{}/start", urlencoded(&exec_id));
			let _ = self
				.client
				.post_json_stream_within(&start_path, &start_cfg, Some(EXEC_START_TIMEOUT))
				.await
				.map_err(|e| map_exec_start_err(e, &opts))?;
			return Ok(());
		}

		// The interactive path takes over here: it needs the connection kept open
		// in both directions, which the request/response client cannot give it.
		if interactive {
			self.exec_interactive(&exec_id).await?;
			return self.exec_exit_status(&exec_id).await;
		}

		let start_cfg = ExecStartConfig {
			detach: false,
			tty: false,
		};
		let start_path = format!("{API_PREFIX}/exec/{}/start", urlencoded(&exec_id));
		let start_resp = self
			.client
			.post_json_stream_within(&start_path, &start_cfg, Some(EXEC_START_TIMEOUT))
			.await
			.map_err(|e| map_exec_start_err(e, &opts))?;
		let mut stream = crate::libpod::parse_multiplexed(start_resp.into_body());

		// Lock stdout once for the whole stream instead of re-acquiring the lock
		// (and issuing a syscall) per frame; stdout is ours exclusively on this
		// path. stderr is locked per frame because the tracing subscriber also
		// writes there: holding its lock across the await loop would starve
		// concurrent log emissions. Flush after each frame so exec streams
		// promptly.
		{
			let mut out = std::io::stdout().lock();
			while let Some(msg) = stream.next().await {
				match msg.map_err(ComposeError::Podman)? {
					LogOutput::StdOut { message } => {
						let _ = out.write_all(String::from_utf8_lossy(&message).as_bytes());
						let _ = out.flush();
					}
					LogOutput::StdErr { message } => {
						let text = String::from_utf8_lossy(&message);
						// Drop the spurious connection-reset teardown frame an OCI
						// exec launch-failure emits, so only the real diagnostic shows.
						if is_exec_teardown_noise(&text) {
							continue;
						}
						let mut err = std::io::stderr().lock();
						let _ = err.write_all(text.as_bytes());
						let _ = err.flush();
					}
				}
			}
		}

		self.exec_exit_status(&exec_id).await
	}

	/// Read the session's exit code and turn a non-zero one into
	/// `RunExited`, so `podup exec` propagates the command's status the way
	/// `docker compose exec` does. Shared by the streaming and interactive
	/// paths: an interactive shell that exits 1 must still exit 1.
	async fn exec_exit_status(&self, exec_id: &str) -> Result<()> {
		let inspect_path = format!("{API_PREFIX}/exec/{}/json", urlencoded(exec_id));
		let inspect: ExecInspect = self
			.client
			.get_json(&inspect_path)
			.await
			.map_err(ComposeError::Podman)?;
		if let Some(code) = inspect.exit_code {
			if code != 0 {
				return Err(ComposeError::RunExited(code));
			}
		}
		Ok(())
	}
}

/// Whether this exec should get a pseudo-terminal and a live stdin.
///
/// Three things must all hold: the caller did not pass `-T`, the caller did not
/// detach, and stdin is actually a terminal. The last is what keeps a scripted
/// `podup exec` on the unchanged streaming path, because allocating a pty for a
/// pipeline would change output framing for every existing script.
///
/// Pure except for the `isatty` probe, which is behind `stdin_is_terminal` so
/// the decision itself is unit-testable.
fn interactive_exec(opts: &ExecOptions) -> bool {
	// Both ends, not just stdin. A pty merges stdout and stderr and writes
	// CRLF, so `podup exec db psql > out.txt` typed at a shell (stdin still a
	// terminal, stdout a file) wrote pty bytes with stderr folded in where it
	// used to write clean demultiplexed output. Measured against
	// `docker compose`, which keeps that redirect clean.
	wants_interactive(opts, stdin_is_terminal() && stdout_is_terminal())
}

/// The decision itself, with the environment passed in.
///
/// Split out so it can be tested: reading stdin here would make every
/// assertion depend on how `cargo test` happened to be invoked.
fn wants_interactive(opts: &ExecOptions, stdin_is_tty: bool) -> bool {
	!opts.no_tty && !opts.detach && stdin_is_tty
}

/// Whether stdout is a terminal.
///
/// A pty merges stdout and stderr and writes CRLF, so allocating one when
/// stdout is redirected changes the bytes a file receives. Asked alongside
/// stdin, never instead of it. On a native Windows console this agrees with the
/// `GetConsoleMode` probe raw mode uses; under an emulated pty (MSYS/Cygwin,
/// e.g. Git Bash) `IsTerminal` can say terminal where raw mode cannot engage,
/// in which case the session degrades to plain streaming rather than failing.
pub(crate) fn stdout_is_terminal() -> bool {
	use std::io::IsTerminal;
	std::io::stdout().is_terminal()
}

/// Whether stdin is a terminal: the gate that keeps a scripted `exec` on the
/// unchanged streaming path.
pub(crate) fn stdin_is_terminal() -> bool {
	use std::io::IsTerminal;
	std::io::stdin().is_terminal()
}

#[cfg(test)]
#[path = "exec_interactive_decision_tests.rs"]
mod interactive_decision_tests;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;

/// The exec create is the endpoint the #1339 drops land on most, and a lost
/// response there cannot be resolved by looking at the container; what is lost
/// is the exec id. Retrying is safe because creating an exec changes nothing:
/// measured on Podman 5.7.0, five unstarted execs leave the container running on
/// the same pid with no extra process inside.
#[cfg(all(test, unix))]
#[path = "exec_create_retry_tests.rs"]
mod exec_create_retry_tests;
