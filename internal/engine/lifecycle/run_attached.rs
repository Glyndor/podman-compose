//! Interactive `run`: a pseudo-terminal on a one-shot container.
//!
//! `podup run -it app bash` parsed `-i` and `-T` and acted on neither: the
//! container got no TTY and no live stdin, so the flags described something that
//! did not happen (#1140, the half of #1079 that did not ship).
//!
//! The order here is the whole point. `attach` opens before `start`, because a
//! container that has already run has already printed, and for a one-shot `run`
//! that missed output is often all the output there was. `exec` does not have
//! this problem (the container is already up) which is why it can attach and
//! start in a single call and this cannot.

use std::time::Duration;

use tokio::io::AsyncReadExt;

use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

/// Upper bound on the kernel-buffer drain when `start` refuses the container.
///
/// The container may have written bytes between the `attach` (which only opens
/// the stream) and the failed `start`; if we drop the hijacked socket without
/// reading, the kernel keeps the buffer until the socket is closed, and the
/// peer sees its `write` block until it does. Bounded so a wedged peer cannot
/// pin the CLI indefinitely.
const START_FAILURE_DRAIN_BUDGET: Duration = Duration::from_secs(2);

impl super::super::Engine {
	/// Attach to a created-but-not-started container, start it, and hand the
	/// terminal over until the command exits. Returns its exit code.
	pub(super) async fn run_attached(&self, container_name: &str) -> Result<i64> {
		// stdin=1 so keystrokes reach the command; stream=1 keeps the connection
		// open both ways. With a TTY the stream is raw, with no 8-byte frame headers,
		// because the pty merges stdout and stderr, which is also why an
		// interactive run cannot separate them, exactly as `podman run -it`.
		let attach_path = format!(
			"{API_PREFIX}/containers/{}/attach?stream=1&stdin=1&stdout=1&stderr=1",
			urlencoded(container_name),
		);
		let hijacked = self
			.client
			.post_hijack(&attach_path, b"")
			.await
			.map_err(ComposeError::Podman)?;

		let start_path = format!(
			"{API_PREFIX}/containers/{}/start",
			urlencoded(container_name),
		);
		if let Err(e) = self.client.post_empty_ok(&start_path).await {
			// The `attach` already opened the stream. The container may have
			// written bytes (startup banners, an error it flushed before the
			// daemon returned 4xx to our `start`, anything the runtime prints
			// before the libpod handler rejects the request) and the kernel
			// keeps that buffer alive until we close or drain the socket.
			// Drop the connection here without reading and the peer blocks on
			// the next `write` until the socket is GC'd; with a long-lived
			// attach the next call can hang. Read until EOF (or the budget
			// below) to let the buffer drain, then return the original error.
			drain_hijacked(hijacked).await;
			return Err(ComposeError::Podman(e));
		}

		// Raw mode only once the container is known to have started; entering it
		// before would leave the caller's terminal unusable behind a failed start.
		self.pump_terminal(
			hijacked,
			&format!("containers/{}", urlencoded(container_name)),
		)
		.await?;

		// The stream ending means the command finished; ask for its status.
		let wait_path = format!(
			"{API_PREFIX}/containers/{}/wait?condition=stopped",
			urlencoded(container_name),
		);
		self.client
			.post_empty_json_unbounded::<i64>(&wait_path)
			.await
			.map_err(ComposeError::Podman)
	}
}

/// Read and discard the kernel buffer on the read side of a hijacked socket
/// until the peer closes it, bounded by [`START_FAILURE_DRAIN_BUDGET`] so a
/// wedged peer cannot pin the caller.
///
/// Used on the `start` failure path of [`Engine::run_attached`]: the success
/// path drains the same buffer through `pump_terminal` while it pumps bytes
/// to the caller's terminal, and the failure path must not enter raw mode or
/// read from stdin (that is the whole reason `pump_terminal` is gated on a
/// successful start) so this is the bounded-read variant that drops the
/// bytes on the floor.
async fn drain_hijacked(hijacked: crate::libpod::client::Hijacked) {
	let mut stream = hijacked.stream;
	let drain = async {
		let mut sink = [0u8; 8 * 1024];
		loop {
			match stream.read(&mut sink).await {
				// EOF: the peer closed the stream. The kernel buffer is empty
				// and the socket can be dropped cleanly.
				Ok(0) => break,
				Ok(_) => continue,
				// A read error means the socket is already gone; further
				// reads would not free anything.
				Err(_) => break,
			}
		}
	};
	let _ = tokio::time::timeout(START_FAILURE_DRAIN_BUDGET, drain).await;
}

impl super::super::Engine {
	/// Attach, start, hand over the terminal, then clean up per `--rm` and map
	/// the exit code. Split from `run` so the interactive tail reads as one
	/// piece.
	pub(super) async fn finish_interactive_run(
		&self,
		run_name: &str,
		rm: bool,
		rm_path: &str,
	) -> Result<()> {
		let outcome = self.run_attached(run_name).await;
		if rm {
			let _ = self.client.delete_ok(rm_path).await;
		}
		match outcome? {
			0 => Ok(()),
			code => Err(ComposeError::RunExited(code)),
		}
	}
}
