use super::*;
use crate::ui::progress::Kind;

fn row(state: State) -> Row {
	Row {
		kind: Kind::Container,
		name: "proj-web-1".to_string(),
		state,
		started: None,
		elapsed: None,
	}
}

fn plain(line: &str) -> String {
	let mut out = String::new();
	let mut chars = line.chars();
	while let Some(c) = chars.next() {
		if c == '\u{1b}' {
			for c in chars.by_ref() {
				if c == 'm' {
					break;
				}
			}
		} else {
			out.push(c);
		}
	}
	out
}

/// The width promise, which the whole repaint rests on: a line that wraps makes
/// the terminal count two rows where the renderer counted one, and every repaint
/// afterwards erases the wrong lines.
#[test]
fn a_line_never_exceeds_the_terminal_width() {
	for width in [10, 20, 30, 40, 80] {
		let r = Row {
			name: "a-very-long-container-name-that-will-not-fit-anywhere".to_string(),
			..row(State::Working("Creating".into()))
		};
		let line = plain(&render(&r, 40, 0, Instant::now(), width));
		assert!(
			line.chars().count() <= width,
			"width {width}: got {} chars: {line:?}",
			line.chars().count()
		);
	}
}

/// The named acceptance test: rendered at width 30 and width 20, every row's
/// visible column count is at most the given width. `…` counts as one column
/// because `fit_cell` measures in chars, and the existing `plain` helper strips
/// escapes the same way it always did (#1672).
#[test]
fn a_row_never_exceeds_the_measured_width() {
	for width in [20, 30] {
		let r = Row {
			name: "a-very-long-container-name-that-will-not-fit-anywhere".to_string(),
			..row(State::Working("Creating".into()))
		};
		let line = plain(&render(&r, 40, 0, Instant::now(), width));
		assert!(
			line.chars().count() <= width,
			"width {width}: got {} chars: {line:?}",
			line.chars().count()
		);
		// And the truncation actually fired: at width 30 the natural row is
		// wider, so the line must have been cut.
		assert!(
			line.contains('\u{2026}'),
			"width {width}: name column should be truncated with `…`: {line:?}"
		);
	}
}

/// The summary line is the row the cursor-up arithmetic has to count too, and
/// the issue names it: a wide summary that wraps makes the repaint
/// erase the wrong lines on every subsequent frame (#1672).
#[test]
fn the_summary_line_never_exceeds_the_measured_width() {
	for width in [5, 10, 20, 30] {
		let s = plain(&summary(3, 100, width));
		assert!(
			s.chars().count() <= width,
			"width {width}: got {} chars: {s:?}",
			s.chars().count()
		);
	}
}

/// A width of zero means "do not truncate": the caller could not read the
/// terminal size. The line is still produced rather than collapsing to nothing.
#[test]
fn width_zero_leaves_the_line_intact() {
	let line = plain(&render(&row(State::Pending), 20, 0, Instant::now(), 0));
	assert!(line.contains("proj-web-1"), "{line:?}");
}

/// The three states are visually distinct without reading the words, which is
/// the entire reason for a marker column.
#[test]
fn each_state_gets_its_own_marker() {
	let now = Instant::now();
	let done = plain(&render(&row(State::Done("Created".into())), 20, 0, now, 80));
	let working = plain(&render(
		&row(State::Working("Creating".into())),
		20,
		0,
		now,
		80,
	));
	let pending = plain(&render(&row(State::Pending), 20, 0, now, 80));
	assert!(done.contains(DONE_MARK), "{done:?}");
	assert!(working.contains(SPINNER[0]), "{working:?}");
	assert!(pending.contains(PENDING_MARK), "{pending:?}");
}

/// A row that closed with the verb "Failed" is not a successful row. The marker
/// is the first thing the eye lands on, so a failure without `✘` (a row that
/// says "Failed" with a green `✔`) is the same contradiction the missing-close
/// fix (#1347) introduced: the verb now says "Failed", and the marker has to
/// match.
#[test]
fn a_failed_row_uses_the_failed_marker_and_not_the_done_marker() {
	let now = Instant::now();
	let line = plain(&render(&row(State::Done("Failed".into())), 20, 0, now, 80));
	assert!(line.contains(FAILED_MARK), "{line:?}");
	assert!(!line.contains(DONE_MARK), "{line:?}");
}

/// Verb case-insensitive: any verb that begins with `fail` is a failure, so a
/// future caller using `"failed"` (lower) or `"Failing"` does not silently
/// regress to the green checkmark.
#[test]
fn a_failed_row_recognises_the_fail_prefix_case_insensitively() {
	let now = Instant::now();
	for verb in ["Failed", "failed", "Failing", "FAIL"] {
		let line = plain(&render(&row(State::Done(verb.into())), 20, 0, now, 80));
		assert!(line.contains(FAILED_MARK), "{verb:?} → {line:?}");
		assert!(!line.contains(DONE_MARK), "{verb:?} → {line:?}");
	}
}

/// The spinner advances with the frame, or a slow pull looks like a hang.
#[test]
fn the_working_marker_advances_with_the_frame() {
	let now = Instant::now();
	let r = row(State::Working("Pulling".into()));
	let a = plain(&render(&r, 20, 0, now, 80));
	let b = plain(&render(&r, 20, 1, now, 80));
	assert_ne!(a, b);
}

/// The frame wraps rather than panicking: the ticker counts up without bound.
#[test]
fn the_frame_index_wraps() {
	let now = Instant::now();
	let r = row(State::Working("Pulling".into()));
	let wrapped = plain(&render(&r, 20, SPINNER.len() * 7 + 3, now, 80));
	let direct = plain(&render(&r, 20, 3, now, 80));
	assert_eq!(wrapped, direct);
}

/// A pending row shows no time. Nothing has taken any, and a `0.0s` there reads
/// as "this was instant" rather than "this has not started".
#[test]
fn a_pending_row_shows_no_time() {
	let line = plain(&render(&row(State::Pending), 20, 0, Instant::now(), 80));
	assert!(!line.contains('s'), "{line:?}");
	assert!(line.contains("Pending"), "{line:?}");
}

/// Sub-minute times keep a decimal, because the interesting range for starting
/// a container is fractions of a second. Past a minute the decimal is noise.
#[test]
fn elapsed_switches_units_at_a_minute() {
	assert_eq!(format_elapsed(Duration::from_millis(100)), "0.1s");
	assert_eq!(format_elapsed(Duration::from_millis(12_900)), "12.9s");
	assert_eq!(format_elapsed(Duration::from_secs(63)), "1m03s");
	assert_eq!(format_elapsed(Duration::from_secs(600)), "10m00s");
}

/// Colour is applied after the line has been fitted. Applied before, the
/// zero-width escapes would be counted as visible columns and the truncation
/// could cut through one, leaving the terminal painted with whatever it set.
#[test]
fn truncation_never_cuts_through_an_escape() {
	let r = Row {
		name: "x".repeat(60),
		..row(State::Working("Creating".into()))
	};
	let line = render(&r, 60, 0, Instant::now(), 24);
	let escapes = line.matches('\u{1b}').count();
	let resets = line.matches("\u{1b}[0m").count();
	assert_eq!(
		escapes,
		resets * 2,
		"every opening escape needs its reset: {line:?}"
	);
}

/// A name carrying an escape sequence cannot repaint the reader's terminal. The
/// name comes from a compose file, which is trusted input, but the row goes
/// through the same `fit_cell` every other table uses rather than trusting that.
#[test]
fn a_name_cannot_drive_the_terminal() {
	let r = Row {
		name: "evil\u{1b}[31m\u{7}name".to_string(),
		..row(State::Pending)
	};
	let line = render(&r, 30, 0, Instant::now(), 80);
	assert!(!line.contains('\u{7}'), "{line:?}");
	assert!(line.contains("name"), "{line:?}");
}

#[test]
fn the_summary_counts_done_over_total() {
	assert_eq!(summary(3, 6, 0), "[+] Running 3/6");
}
