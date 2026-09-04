//! What the buildah stream says, read line by line.
//!
//! The libpod build endpoint answers with buildah's own output, one JSON line
//! per text line. Three shapes matter to the board: `STEP n/m: <instruction>`,
//! which moves the image row's verb; the bare 64-hex image id buildah prints
//! after `Successfully tagged`, which a script reading `build` from a pipe
//! wants on stdout; and the failure path, where the stream a terminal folded
//! away is replayed once so the reason is on screen (#1681).
use std::io::IsTerminal;

use crate::engine::Engine;
use crate::error::ComposeError;

impl Engine {
	/// Close the row as `Failed`, then on a terminal replay the full stream
	/// as scrollback so the failure reason is on screen. In a pipe every line
	/// has already been written by `note_for` and no replay is needed.
	pub(super) fn fail_build(
		&self,
		tag: &str,
		err: String,
		capture: Vec<String>,
		quiet: bool,
	) -> ComposeError {
		if !quiet {
			crate::ui::progress_line("Image", tag, "Failed");
		}
		if !quiet && std::io::stderr().is_terminal() {
			use std::io::Write;
			let mut out = std::io::stderr().lock();
			for line in &capture {
				let _ = writeln!(out, "{tag} | {line}");
			}
		}
		ComposeError::Build(err)
	}
}

/// Whether a buildah stream line is the one that carries the new image id.
///
/// Buildah closes a successful build with the full image id on a line of its
/// own: 64 hex digits, nothing else. It is the second-to-last line of the
/// stream, between `Successfully tagged <tag>` and `Successfully built
/// <short-id>` (measured on Podman 5.7, 2026-09-04). The `--> <short-id>`
/// layer markers and `--> Using cache <digest>` carry a prefix and are not
/// matched. A script reading `podup build` from a pipe wants exactly this
/// value, and a terminal does not (#1681).
pub(super) fn parse_image_id_line(line: &str) -> Option<String> {
	let line = line.trim();
	if line.len() != 64 || !line.bytes().all(|b| b.is_ascii_hexdigit()) {
		return None;
	}
	Some(line.to_string())
}

/// Parse one buildah stream line into the row verb it implies, if any.
///
/// On the libpod build stream a `STEP n/m: <instruction>` line arrives once
/// per Dockerfile instruction: the row's verb should reflect where in the
/// build we are. Every other line (`--> 3f3c...`, `COMMIT <tag>`,
/// `Successfully tagged <tag>`) carries no transition of its own; those are
/// routed through `progress::note_for` as tail lines.
#[derive(Default)]
pub(super) struct BuildStreamProgress {
	last_step: Option<(usize, usize)>,
}

impl BuildStreamProgress {
	pub(super) fn new() -> Self {
		Self::default()
	}

	pub(super) fn observe(&mut self, line: &str) -> Option<String> {
		const STEP_PREFIX: &str = "STEP ";
		let line = line.trim_end();
		let rest = line.strip_prefix(STEP_PREFIX)?;
		// `STEP n/m: <instruction>`. The colon separates the counters from
		// the instruction text. Both sides must parse.
		let (counters, _) = rest.split_once(':')?;
		let (cur, total) = counters.split_once('/')?;
		let cur: usize = cur.parse().ok()?;
		let total: usize = total.parse().ok()?;
		self.last_step = Some((cur, total));
		Some(self.format_verb())
	}

	fn format_verb(&self) -> String {
		match self.last_step {
			Some((cur, total)) => format!("Building {cur}/{total}"),
			None => "Building".to_string(),
		}
	}
}
