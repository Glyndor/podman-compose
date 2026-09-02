//! Per-line log prefixing for `docker compose logs`-style multi-service output.

use std::io::Write;

/// Cap on the buffered partial line. A container that emits a very long run
/// with no newline — a `\r`-updated progress bar, binary output, a pathological
/// single line — must not grow `pending` without bound. At this size the
/// partial is flushed as its own prefixed line (as docker does) rather than
/// held in memory forever.
const MAX_PENDING: usize = 64 * 1024;

/// The palette slot for a log-prefix label.
///
/// Delegates to [`crate::ui::identity_slot`], never `crate::ui::service_slot`
/// directly: the label reaching this module still carries its replica suffix
/// (`web-1`, from `display_label`), and only `identity_slot` strips that (and
/// a project prefix) before resolving through the per-project colour registry
/// `set_services` fills. Resolving `service_slot` against the raw label
/// bypassed the registry entirely — that key was never in it — so every log
/// prefix silently fell back to the per-label hash instead of the sequential
/// colour `ps` uses. Measured on an 8-service project: `ps` gave 8 distinct
/// colours, `logs` only 6, before this indirection existed.
///
/// [`prefix_style`] renders this into a [`crate::ui::Style`] and nothing else
/// — so the routing choice this function makes is the one thing a regression
/// test needs to pin down, and it can compare the slot directly rather than a
/// rendered `Style`. That distinction matters: the narrow (6-colour) fallback
/// wraps a slot index mod 6, so two different wide-palette slots can render
/// identically there, which would silently swallow a routing regression that
/// a `Style` comparison alone could not see.
fn prefix_slot(label: &str) -> usize {
	crate::ui::identity_slot(label)
}

/// The identity colour for a log-prefix label. See [`prefix_slot`] for the
/// routing decision this renders.
///
/// Kept as its own function (rather than calling [`crate::ui::identity_style`]
/// inline in [`LinePrefixer::new`]) so the routing choice is unit-testable on
/// its own: [`LinePrefixer::new`]'s final output is gated by `stdout_colored`,
/// which is false under the test harness (stdout is not a TTY there), so no
/// test that only inspects a built `LinePrefixer`'s output can ever observe
/// which slot function was called.
fn prefix_style(label: &str) -> crate::ui::Style {
	crate::ui::style_for_slot(prefix_slot(label))
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
#[path = "log_prefix_tests.rs"]
mod tests;
