//! Per-line log prefixing for `docker compose logs`-style multi-service output.

use std::io::Write;

/// Cap on the buffered partial line. A container that emits a very long run
/// with no newline — a `\r`-updated progress bar, binary output, a pathological
/// single line — must not grow `pending` without bound. At this size the
/// partial is flushed as its own prefixed line (as docker does) rather than
/// held in memory forever.
const MAX_PENDING: usize = 64 * 1024;

/// The identity colour for a log-prefix label.
///
/// Delegates to [`crate::ui::identity_style`], never `crate::ui::service_style`
/// directly: the label reaching this module still carries its replica suffix
/// (`web-1`, from `display_label`), and only `identity_style` strips that (and
/// a project prefix) before resolving through the per-project colour registry
/// `set_services` fills. Calling `service_style` on the raw label bypassed the
/// registry entirely — that key was never in it — so every log prefix silently
/// fell back to the per-label hash instead of the sequential colour `ps` uses.
/// Measured on an 8-service project: `ps` gave 8 distinct colours, `logs` only
/// 6, before this indirection existed.
///
/// Kept as its own function (rather than calling `identity_style` inline in
/// [`LinePrefixer::new`]) so the routing choice is unit-testable on its own:
/// [`LinePrefixer::new`]'s final output is gated by `stdout_colored`, which is
/// false under the test harness (stdout is not a TTY there), so no test that
/// only inspects a built `LinePrefixer`'s output can ever observe which style
/// function was called.
fn prefix_style(label: &str) -> crate::ui::Style {
	crate::ui::identity_style(label)
}

/// Tags each complete log line with `{label} | `, the way `docker compose logs`
/// labels multi-service output. Bytes arrive as stream frames that may split a
/// line across frames, so a partial line is buffered until its newline arrives
/// (up to [`MAX_PENDING`]).
pub(super) struct LinePrefixer {
	label: String,
	pending: Vec<u8>,
}

impl LinePrefixer {
	/// Build a prefixer for `label`. `prefix` gates whether any `{label} | ` is
	/// emitted at all (`logs --no-log-prefix`); `allow_color` gates the colour of
	/// the prefix (`logs --no-color`), still subject to stdout being a colour sink.
	pub(super) fn new(label: &str, prefix: bool, allow_color: bool) -> Self {
		// `--no-log-prefix`: emit the bare line with no `{label} | ` tag.
		if !prefix {
			return Self {
				label: String::new(),
				pending: Vec::new(),
			};
		}
		// Colour the whole prefix with the service's stable colour so aggregated
		// multi-service output is easy to scan. Gated on stdout being a colour sink
		// (a raw write anstream does not strip for us) and on `--no-color`.
		// One space before the bar, not two. Attached `up` already prints
		// `{prefix} | ` with one, so the same container was tagged two different
		// ways by two commands in the same binary — and anything parsing the
		// prefix had to accept both. docker compose uses one space too.
		let plain = format!("{label} | ");
		let label = crate::ui::paint(
			prefix_style(label),
			&plain,
			allow_color && crate::ui::stdout_colored(),
		);
		Self {
			label,
			pending: Vec::new(),
		}
	}

	/// Buffer `chunk` and write every complete line it now completes.
	///
	/// Returns `Err` when the sink is gone. Every write used to be discarded with
	/// `let _ =`, so a reader that closed the pipe — `logs -f | head`,
	/// `| grep -q`, `| less` and quit — was never noticed and the follow loop
	/// streamed into a dead pipe until the process was killed. The error is
	/// returned rather than handled here so the caller decides: a broken pipe is
	/// a clean end of output, any other io error is a real failure.
	pub(super) fn write(&mut self, out: &mut impl Write, chunk: &[u8]) -> std::io::Result<()> {
		self.pending.extend_from_slice(chunk);
		while let Some(nl) = self.pending.iter().position(|&b| b == b'\n') {
			out.write_all(self.label.as_bytes())?;
			out.write_all(&self.pending[..=nl])?;
			self.pending.drain(..=nl);
		}
		// The remaining bytes are a partial line with no newline yet. Bound it:
		// a container spewing without a newline (a `\r` progress bar, binary
		// data) would otherwise grow `pending` without limit. Break the
		// over-long partial into its own prefixed line and start fresh.
		if self.pending.len() >= MAX_PENDING {
			out.write_all(self.label.as_bytes())?;
			out.write_all(&self.pending)?;
			out.write_all(b"\n")?;
			self.pending.clear();
		}
		out.flush()
	}

	/// Flush a trailing line that never received a newline (e.g. at stream end).
	///
	/// Best-effort: this runs after the stream is done, so a sink that has gone
	/// away has nothing left to tell the caller.
	pub(super) fn flush_tail(&mut self, out: &mut impl Write) {
		if !self.pending.is_empty() {
			let _ = out.write_all(self.label.as_bytes());
			let _ = out.write_all(&self.pending);
			let _ = out.write_all(b"\n");
			let _ = out.flush();
			self.pending.clear();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::{prefix_style, LinePrefixer};

	/// The regression this guards: `new` used to call `service_style(label)`
	/// directly on the raw, replica-suffixed label, which is never a key in the
	/// per-project registry `identity_style` resolves against — every log
	/// prefix silently fell back to the hash instead of the sequential colour
	/// `ps` uses for the same container. `identity_style` strips the `-N`
	/// replica suffix before resolving, so `prefix_style("web-1")` must land on
	/// the exact same colour as `identity_style("web")` applied to the same
	/// container — and, since the raw hash of the two strings differs, must
	/// disagree with `service_style("web-1")` applied to the untouched label.
	#[test]
	fn prefix_style_routes_through_identity_style_not_service_style() {
		assert_eq!(
			prefix_style("web-1"),
			crate::ui::identity_style("web"),
			"the replica suffix must be stripped before resolving the colour"
		);
		assert_ne!(
			prefix_style("web-1"),
			crate::ui::service_style("web-1"),
			"the raw, suffixed label must not be hashed directly"
		);
	}

	/// #1082: one space before the bar. Attached `up` already used one, so the
	/// same container was tagged two different ways by two commands in the same
	/// binary; docker compose uses one too.
	#[test]
	fn line_prefixer_tags_lines_and_buffers_partials() {
		let mut p = LinePrefixer::new("web", true, false);
		let mut out: Vec<u8> = Vec::new();
		p.write(&mut out, b"hello\nwor").unwrap();
		// The complete line is tagged; the partial "wor" waits for its newline.
		assert_eq!(out, b"web | hello\n");
		p.write(&mut out, b"ld\n").unwrap();
		assert_eq!(out, b"web | hello\nweb | world\n");
	}

	#[test]
	fn line_prefixer_flush_tail_emits_unterminated_line() {
		let mut p = LinePrefixer::new("db", true, false);
		let mut out: Vec<u8> = Vec::new();
		p.write(&mut out, b"partial").unwrap();
		assert!(out.is_empty(), "a line with no newline is held back");
		p.flush_tail(&mut out);
		assert_eq!(out, b"db | partial\n");
	}

	#[test]
	fn line_prefixer_bounds_a_newlineless_flood() {
		use super::MAX_PENDING;
		let mut p = LinePrefixer::new("web", true, false);
		let mut out: Vec<u8> = Vec::new();
		// Feed more than the cap with no newline in sight, in small chunks.
		let chunk = vec![b'x'; 4096];
		for _ in 0..((MAX_PENDING / chunk.len()) + 2) {
			p.write(&mut out, &chunk).unwrap();
		}
		// The partial was flushed as a prefixed line instead of being buffered
		// unbounded, and nothing is left pending beyond the last sub-cap chunk.
		assert!(
			!out.is_empty(),
			"the over-long partial was emitted, not held"
		);
		assert!(
			p.pending.len() < MAX_PENDING,
			"pending stays bounded under the cap, was {}",
			p.pending.len()
		);
		assert!(
			out.starts_with(b"web | "),
			"the flushed partial is prefixed"
		);
	}

	#[test]
	fn line_prefixer_no_prefix_emits_bare_lines() {
		// `--no-log-prefix`: lines pass through with no `{label} | ` tag.
		let mut p = LinePrefixer::new("web", false, false);
		let mut out: Vec<u8> = Vec::new();
		p.write(&mut out, b"hello\n").unwrap();
		assert_eq!(out, b"hello\n");
		p.write(&mut out, b"tail").unwrap();
		p.flush_tail(&mut out);
		assert_eq!(out, b"hello\ntail\n");
	}

	/// #1102: a sink that has gone away must surface as an error, not be
	/// swallowed. Every write here used to be `let _ =`, so `logs -f | head`
	/// streamed into a closed pipe forever instead of exiting.
	#[test]
	fn line_prefixer_surfaces_a_broken_pipe() {
		/// A writer that refuses everything the way a closed pipe does.
		struct ClosedPipe;
		impl std::io::Write for ClosedPipe {
			fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
				Err(std::io::Error::new(
					std::io::ErrorKind::BrokenPipe,
					"broken pipe",
				))
			}
			fn flush(&mut self) -> std::io::Result<()> {
				Ok(())
			}
		}

		let mut p = LinePrefixer::new("web", true, false);
		let err = p
			.write(&mut ClosedPipe, b"hello\n")
			.expect_err("a closed sink must be reported");
		assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
	}
}
