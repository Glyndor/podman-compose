//! Interactive `exec`: a pseudo-terminal and a live stdin.
//!
//! `podup exec -it db psql` is the most-used shell-into-a-container habit in
//! compose, and it did not work: podup allocated no TTY and attached no stdin,
//! so `-i` set a flag nothing read and `-T` disabled something that never
//! existed (#1079). Unix shipped first; Windows followed once the hijack ran
//! over the named pipe and raw mode spoke the console API (#1154). The
//! non-interactive path is unchanged everywhere, so `podup exec` in a script
//! behaves exactly as before on every platform.

use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::Engine;

impl Engine {
	/// Run an exec session with a pty and a live stdin.
	///
	/// The terminal is restored by `RawMode`'s `Drop`, so every exit path
	/// (command exits, socket dies, `?` on an unrelated error, panic) leaves the
	/// caller's shell usable. That is the one thing this must not get wrong: a
	/// terminal left raw has no echo and no line discipline, and the user cannot
	/// even see what they type to fix it.
	pub(super) async fn exec_interactive(&self, exec_id: &str) -> Result<()> {
		let start_path = format!("{API_PREFIX}/exec/{}/start", urlencoded(exec_id));
		let body = br#"{"Detach":false,"Tty":true}"#;
		let hijacked = self
			.client
			.post_hijack(&start_path, body)
			.await
			.map_err(ComposeError::Podman)?;

		// Raw mode, sizing and the byte pump all live in `terminal_pump`, shared
		// with interactive `run` so the loop that must not get raw mode wrong
		// exists once.
		self.pump_terminal(hijacked, &format!("exec/{}", urlencoded(exec_id)))
			.await
	}
}
