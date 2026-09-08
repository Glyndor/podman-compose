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
use crate::ui::{fit_cell, identity_style, paint, stderr_colored, AnsiColor, Style};

/// Frames of the working marker. Ten braille cells, so no dependency and no font
/// assumption beyond what every terminal that reports 256 colours already has.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Marker for a finished row.
const DONE_MARK: &str = "✔";

/// Marker for a row that finished in error. Distinct from [`DONE_MARK`] so a
/// failed row cannot be mistaken for a successful one at a glance: the verb
/// already says "Failed", but the row before the user shows the green checkmark
/// of a row that closed cleanly, which is the contradiction #1347 introduced
/// when the missing-close sites started sending `"Failed"` as the closing verb.
const FAILED_MARK: &str = "✘";

/// Marker for a row nothing has happened to yet.
const PENDING_MARK: &str = "⠿";

/// Width of the resource-noun column. `Container` is the longest of the five.
const KIND_WIDTH: usize = 9;

/// Width reserved for the elapsed time, right-aligned so the digits line up
/// rather than drifting with each value's length.
const TIME_WIDTH: usize = 6;

/// Width the verb column takes when nothing has trimmed it. Twelve covers the
/// longest verb in use today (`Healthcheck`), so a healthy row never has to
/// truncate its verb.
const VERB_WIDTH: usize = 12;

/// Columns between the kind column and the verb column: one space, the name
/// (variable), one space.
const NAME_GUTTER: usize = 2;

/// Columns the row carries without the variable ones: leading space, marker,
/// space, kind, then the time column with the spaces that bracket it.
const KIND_AND_TIME_OVERHEAD: usize = 1 + 1 + 1 + KIND_WIDTH + 1 + 1 + TIME_WIDTH;

/// The minimum width that any row may be rendered at. Below this, truncation
/// gives up and the line is hard-cut: a row at width 5 has only enough room
/// for the kind column, and the verb is the first thing that has to go.
const MIN_NAME_WIDTH: usize = 1;

/// The marker for a row, and the style it carries.
///
/// Colour here is state, not identity: a finished row is green because something
/// now exists, a pending one is dim because nothing has happened to it. The name
/// beside it keeps its identity colour, which is why the marker must not reuse
/// that palette. A failed row breaks the green-equals-success tie by carrying
/// the verb "Failed"; the marker reads it too, so a failure and a success do
/// not share a glyph.
fn marker(row: &Row, frame: usize) -> (&'static str, Style) {
	match &row.state {
		State::Done(verb) if verb.to_ascii_lowercase().starts_with("fail") => (
			FAILED_MARK,
			Style::new().fg_color(Some(AnsiColor::Red.into())),
		),
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
///
/// Truncation order is name first (with `…`), then verb, leaving the elapsed
/// column intact as long as it fits. The fixed columns (mark, kind, time) are
/// never shortened: they are the part that says what the row is and how long
/// it took, and a `…` in the elapsed column would be misread as a duration.
pub fn render(row: &Row, name_width: usize, frame: usize, now: Instant, width: usize) -> String {
	let (mark, mark_style) = marker(row, frame);
	let time = row.duration(now).map(format_elapsed).unwrap_or_default();
	let verb_text = verb(row);
	let (name_cell, verb_cell) = fit_columns(&row.name, name_width, verb_text, width);
	// `name_cell` already carries its width (fit_cell padded or truncated to
	// the budget we asked for), and the verb cell follows it without a gap so
	// the verb column lines up under the time column. The verb is padded to
	// `VERB_WIDTH` only when there is room; a verb trimmed to its character
	// count just rides at its natural width.
	let verb_field = if verb_cell.chars().count() == VERB_WIDTH {
		format!("{verb_cell:<VERB_WIDTH$}")
	} else {
		verb_cell
	};
	let mut line = format!(
		" {mark} {:<KIND_WIDTH$} {name_cell} {verb_field} {time:>TIME_WIDTH$}",
		row.kind.noun(),
	);
	line.truncate(line.trim_end().len());
	let line = if width > 0 && line.chars().count() > width {
		// Hard guarantee: never exceed width, even when fit_columns' estimate
		// disagreed with the format by a column or two. A `…` here would lie
		// about the duration, so the truncation just drops characters.
		line.chars().take(width).collect()
	} else {
		line
	};
	colourise(&line, row, mark, mark_style, stderr_colored())
}

/// Width-budgeted name and verb cells. Shrinks the name first (with `…`), then
/// the verb, keeping the elapsed column alive.
fn fit_columns(name: &str, name_width: usize, verb_text: &str, width: usize) -> (String, String) {
	// `width == 0` is the "don't truncate" signal; render at natural width.
	if width == 0 {
		return (fit_cell(name, name_width), verb_text.to_string());
	}
	// How many columns the natural-width row would take.
	let natural = KIND_AND_TIME_OVERHEAD + name_width + NAME_GUTTER + VERB_WIDTH;
	if natural <= width {
		return (fit_cell(name, name_width), verb_text.to_string());
	}
	let over = natural - width;
	// Shrink the name first. The minimum is one column for `…`.
	let name_shrink = over.min(name_width.saturating_sub(MIN_NAME_WIDTH));
	let new_name_width = name_width - name_shrink;
	let name_cell = fit_cell(name, new_name_width);
	let remaining = over - name_shrink;
	// Then the verb. The verb is plain text (no `…`), so the minimum is zero ,
	// if it is shorter than the budget, no padding is added.
	let verb_chars = verb_text.chars().count();
	let verb_shrink = remaining.min(verb_chars);
	let verb_cell: String = if verb_shrink >= verb_chars {
		String::new()
	} else {
		verb_text.chars().take(verb_chars - verb_shrink).collect()
	};
	(name_cell, verb_cell)
}

/// Re-apply colour to an already-fitted line.
///
/// Deliberately done after fitting rather than before: the escape sequences are
/// zero-width, so measuring a coloured line counts characters the terminal never
/// draws, and the truncation would cut in the middle of an escape and leave the
/// terminal painted. Gated on `colour` (the resolved colour choice): when the
/// caller asked for no styling (NO_COLOR=1, --ansi never) the mark and the name
/// keep their plain glyphs, and the live region keeps repainting under
/// `script -qfc` without an escape stream a reader has to strip.
fn colourise(plain: &str, row: &Row, mark: &str, mark_style: Style, colour: bool) -> String {
	if !colour {
		return plain.to_string();
	}
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
/// many. Fitted to `width` like the rows, so a narrow terminal that truncates
/// the rows also truncates the summary and never repaints over the wrong lines.
pub fn summary(done: usize, total: usize, width: usize) -> String {
	let s = format!("[+] Running {done}/{total}");
	if width > 0 && s.chars().count() > width {
		s.chars().take(width).collect()
	} else {
		s
	}
}

/// A dimmed tail line painted under a working row to give the reader a peek
/// at what the producer is currently emitting. One leading space keeps it
/// visually under the marker column rather than butting against it, and the
/// rest is plain text the caller has already collapsed to a single line.
/// Truncated by character, not by escape sequence, the same way [`render`]
/// does it.
pub fn render_note(line: &str, width: usize) -> String {
	// The invariant this function relies on, asserted where it is relied on
	// rather than only where it is produced. A note carrying `\n` is drawn as
	// several rows and counted as one, and the repaint arithmetic then erases
	// the wrong lines (#1733). `note_for` splits at ingestion; this catches a
	// future path that does not.
	debug_assert!(!line.contains('\n'), "a note must be one line: {line:?}");
	let indented = format!(" {line}");
	let trimmed = if width > 0 && indented.chars().count() > width {
		indented.chars().take(width).collect::<String>()
	} else {
		indented
	};
	let plain = trimmed.trim_end();
	if !stderr_colored() {
		return plain.to_string();
	}
	paint(Style::new().dimmed(), plain, true)
}

#[cfg(test)]
#[path = "row_tests.rs"]
mod tests;
