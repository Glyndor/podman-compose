//! Turning one [`Row`](super::Row) into one line of text.
//!
//! Pure, and separate from the terminal driver on purpose: the arithmetic that
//! decides how wide a line is, is the same arithmetic the cursor-up repaint
//! depends on. A line that is one column too long wraps, the terminal counts two
//! rows where the renderer counted one, and every subsequent repaint erases the
//! wrong lines. So the width rule is tested on its own rather than inferred from
//! a screenshot.

use std::time::{Duration, Instant};

use super::{Row, State};
use crate::ui::{fit_cell, identity_style, paint, AnsiColor, Style};

/// Frames of the working marker. Ten braille cells, so no dependency and no font
/// assumption beyond what every terminal that reports 256 colours already has.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Marker for a finished row.
const DONE_MARK: &str = "✔";

/// Marker for a row nothing has happened to yet.
const PENDING_MARK: &str = "⠿";

/// Width of the resource-noun column. `Container` is the longest of the five.
const KIND_WIDTH: usize = 9;

/// Width reserved for the elapsed time, right-aligned so the digits line up
/// rather than drifting with each value's length.
const TIME_WIDTH: usize = 6;

/// The marker for a row, and the style it carries.
///
/// Colour here is state, not identity: a finished row is green because something
/// now exists, a pending one is dim because nothing has happened to it. The name
/// beside it keeps its identity colour, which is why the marker must not reuse
/// that palette.
fn marker(row: &Row, frame: usize) -> (&'static str, Style) {
	match row.state {
		State::Done(_) => (
			DONE_MARK,
			Style::new().fg_color(Some(AnsiColor::Green.into())),
		),
		State::Working(_) => (SPINNER[frame % SPINNER.len()], Style::new()),
		State::Pending => (PENDING_MARK, Style::new().dimmed()),
	}
}

/// The verb shown for a row.
fn verb(row: &Row) -> &str {
	match &row.state {
		State::Done(v) | State::Working(v) => v,
		State::Pending => "Pending",
	}
}

/// `0.4s`, `12.9s`, `1m03s`. Sub-minute values keep a decimal because the
/// interesting range for a container start is fractions of a second; past a
/// minute the decimal is noise.
pub fn format_elapsed(d: Duration) -> String {
	let secs = d.as_secs_f64();
	if secs < 60.0 {
		format!("{secs:.1}s")
	} else {
		format!("{}m{:02}s", d.as_secs() / 60, d.as_secs() % 60)
	}
}

/// One board line, fitted to `width` columns.
///
/// `width` is the terminal's, and the returned line never exceeds it: the
/// cursor-up repaint counts one terminal row per line it wrote, and a line that
/// wraps makes that count wrong for every repaint afterwards.
pub fn render(row: &Row, name_width: usize, frame: usize, now: Instant, width: usize) -> String {
	let (mark, mark_style) = marker(row, frame);
	let time = row.duration(now).map(format_elapsed).unwrap_or_default();
	let plain = format!(
		" {mark} {:<KIND_WIDTH$} {} {:<12} {time:>TIME_WIDTH$}",
		row.kind.noun(),
		fit_cell(&row.name, name_width),
		verb(row),
	);
	let plain = fit_cell(plain.trim_end(), 0);
	let plain = if width > 0 && plain.chars().count() > width {
		fit_cell(&plain, width)
	} else {
		plain
	};
	colourise(&plain, row, mark, mark_style)
}

/// Re-apply colour to an already-fitted line.
///
/// Deliberately done after fitting rather than before: the escape sequences are
/// zero-width, so measuring a coloured line counts characters the terminal never
/// draws, and the truncation would cut in the middle of an escape and leave the
/// terminal painted.
fn colourise(plain: &str, row: &Row, mark: &str, mark_style: Style) -> String {
	let mut out = plain.to_string();
	if let Some(pos) = out.find(mark) {
		let styled = paint(mark_style, mark, true);
		out.replace_range(pos..pos + mark.len(), &styled);
	}
	if let Some(pos) = out.find(&row.name) {
		let styled = paint(identity_style(&row.name), &row.name, true);
		out.replace_range(pos..pos + row.name.len(), &styled);
	}
	out
}

/// The summary line above the region: how many resources are done out of how
/// many.
pub fn summary(done: usize, total: usize) -> String {
	format!("[+] Running {done}/{total}")
}

#[cfg(test)]
#[path = "row_tests.rs"]
mod tests;
