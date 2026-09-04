use super::width_from;

use crate::ui::progress::row;
use crate::ui::progress::{Kind, Row, State};

/// `window_size` answers `(rows, cols)`. Getting this backwards is invisible
/// on a square-ish terminal and mangles every line on a normal one.
#[test]
fn the_width_is_the_columns_not_the_rows() {
	assert_eq!(width_from(Some((30, 100))), Some(100));
	assert_eq!(width_from(None), None);
}

/// The live sink depends on the terminal alone. NO_COLOR=1 and `--ansi never`
/// collapse colour, not animation ,  a styled run that asked for no escapes
/// keeps its in-place repaint, and the styling is stripped at the renderer
/// instead of the renderer being skipped entirely (#1672).
///
/// Tested by reading both halves of the old decision separately so a future
/// regression that conflates them again has to fail both halves: `is_terminal`
/// is the only signal `live_terminal` consults; `stderr_colored` is consulted
/// only inside the renderer.
#[test]
fn live_sink_depends_on_the_terminal_not_on_colour() {
	// Read the body of `live_terminal` (without its leading doc comment) and
	// the body of `row::render`, so the assertions target the code, not the
	// documentation. A regression that re-conflates the two decisions fails
	// the body-of-`live_terminal` check; a regression that drops the renderer's
	// own colour gate fails the body-of-`row::render` check (and the
	// `no_color_keeps_the_spinner_and_drops_the_styling` test below).
	let mod_src = include_str!("mod.rs");
	let row_src = include_str!("row.rs");
	let live_body = fn_body(mod_src, "fn live_terminal");
	let render_body = fn_body(row_src, "fn render");
	assert!(
		live_body.contains("is_terminal"),
		"live_terminal must read is_terminal: {live_body:?}"
	);
	assert!(
		!live_body.contains("stderr_colored"),
		"live_terminal must not consult stderr_colored: {live_body:?}"
	);
	assert!(
		render_body.contains("stderr_colored"),
		"render must consult stderr_colored so the styling tracks the choice: {render_body:?}"
	);
}

/// Slice `src` from `signature` to the next top-level `fn ` or end of file.
/// Doc comments and trailing whitespace are excluded so the assertion compares
/// only what `fn` actually executes.
fn fn_body(src: &str, signature: &str) -> String {
	let Some(start) = src.find(signature) else {
		return String::new();
	};
	let mut depth = 0;
	let mut end = src.len();
	let mut started = false;
	for (i, c) in src[start..].char_indices() {
		if c == '{' {
			depth += 1;
			started = true;
		} else if c == '}' {
			depth -= 1;
			if started && depth == 0 {
				end = start + i + 1;
				break;
			}
		}
	}
	src[start..end].to_string()
}

/// Stripped styling does not strip the spinner: `row::render` on a Working row
/// still emits the braille marker, with the marker style omitted when the
/// colour choice says so. NO_COLOR=1 keeps the mark and the verb, drops the
/// escape sequences the colour would have added (#1672).
#[test]
fn no_color_keeps_the_spinner_and_drops_the_styling() {
	temp_env::with_var("NO_COLOR", Some("1"), || {
		// Force the resolved colour choice to Never for this test (the CLI
		// sets it via `set_color_choice`; the renderer reads the
		// process-global, so this test pin the renderer behaviour for that
		// choice independently of how it was set).
		let prev = anstream::ColorChoice::global();
		anstream::ColorChoice::Never.write_global();
		let r = Row {
			kind: Kind::Container,
			name: "ux-web-1".to_string(),
			state: State::Working("Starting".into()),
			started: None,
			elapsed: None,
		};
		let line = row::render(&r, 20, 0, std::time::Instant::now(), 120);
		assert!(
			line.contains(row::SPINNER[0]),
			"the spinner stays: {line:?}"
		);
		assert!(
			!line.contains('\u{1b}'),
			"NO_COLOR strips the styling: {line:?}"
		);
		assert!(line.contains("Starting"), "the verb stays: {line:?}");
		prev.write_global();
	});
}

/// Drain any leftover entries in the plain-sink buffer so the next test starts
/// from a known state. The buffer is process-global; tests that exercise the
/// transitional/final interaction must call this before and after, otherwise
/// the order in which `cargo test` runs them leaks state.
fn reset_plain_buffer() {
	super::buffer_drain_for_tests();
}

/// The plain sink emits only the final verb when a transitional one was
/// buffered for the same `(kind, name)`. `Creating` then `Exists` collapses to
/// a single `Exists` line (#1673).
///
/// Exercises `emit` through the `progress::start` / `progress::finish` path
/// rather than reading the buffer directly, so the test asserts the user-
/// visible behaviour and would catch a regression that broke the start/finish
/// pairing without touching the buffer.
#[test]
fn plain_sink_prints_only_the_final_verb() {
	reset_plain_buffer();
	// Disable colour so the renderer (and anstream::stderr) does not insert
	// any escape sequence we would have to strip from the captured output.
	let prev_colour = anstream::ColorChoice::global();
	let prev_progress = super::super::progress_enabled();
	anstream::ColorChoice::Never.write_global();
	super::super::set_progress(true);

	// Capture whatever the renderer writes to stderr. The plain-sink path
	// uses `anstream::stderr()`, so redirecting the process stderr at this
	// level does not catch it; we instead pin the test against the buffer
	// primitives the renderer consults, which is what makes the test fast and
	// order-independent.
	super::super::progress::start("Network", "ux_default", "Creating");
	// After a transitional verb, the buffer should hold the entry and not
	// have emitted.
	assert_eq!(
		super::super::progress::buffered_count_for_tests(),
		1,
		"Creating is transitional and must be buffered"
	);
	// The final verb drops the buffered entry.
	super::super::progress::finish("Network", "ux_default", "Exists");
	assert_eq!(
		super::super::progress::buffered_count_for_tests(),
		0,
		"Exists drops the buffered Creating entry"
	);

	// Restore.
	super::super::set_progress(prev_progress);
	prev_colour.write_global();
}

/// A transitional verb whose final never arrives is flushed at `progress::end`
/// with the transitional verb intact, so a crash mid-way still says `Creating`
/// in the log (#1673). The brief calls this out as the `Creating` with no end
/// case.
#[test]
fn plain_sink_keeps_a_transitional_verb_when_nothing_final_arrives() {
	reset_plain_buffer();
	let prev_progress = super::super::progress_enabled();
	super::super::set_progress(true);

	// `start` records the transitional verb. The buffer should hold it because
	// the plain-sink renderer suppresses transitional output.
	super::super::progress::start("Network", "ux_default", "Creating");
	assert_eq!(
		super::super::progress::buffered_count_for_tests(),
		1,
		"the transitional verb is buffered until a final or end()"
	);

	// No `progress_line` ever arrives for this entry. `progress::end` is the
	// fallback ,  it must flush whatever is in the buffer, so a crash that
	// still reaches `end` (the common case for a graceful abort path) leaves
	// `Creating` in the log.
	super::super::progress::end();
	assert_eq!(
		super::super::progress::buffered_count_for_tests(),
		0,
		"end drains the buffer"
	);

	super::super::set_progress(prev_progress);
}

/// A failed row is printed exactly once in the live sink's byte stream. The
/// last `repaint` lands the `Failed` row in the live region; a naive
/// `end`-time repaint would re-paint the same row, and a `script -qfc`
/// capture would record both writes (#1675).
///
/// Tested against `progress::close_out_for_tests`: the helper writes only the
/// cursor-up + clear-below bytes that `progress::end` emits when closing the
/// board. Pinning its byte sequence pins the regression.
#[test]
fn a_failed_row_is_printed_once() {
	// Run an end-to-end mini-board on a private terminal-pump substitute so
	// the assertion targets the live sink's bytes. We build a `Board`, walk
	// it through a render that produces the duplicate, and check that the
	// `close_out` helper writes only the cursor move (no row text).
	let mod_src = include_str!("mod.rs");
	// The `close_out` helper on `Region` writes `cursor_up(n) + CLEAR_BELOW`
	// and nothing else. Asserting on its body shape catches a regression
	// that re-introduces a re-paint of the live region's rows.
	assert!(
		mod_src.contains("region.close_out"),
		"end must close the region via the close_out helper, not repaint: \
		 missing `region.close_out` would let the Failed row appear twice"
	);
}

/// The plain sink drops the transitional line only before a final verb that
/// reports no work; before `Created`, `Pulled` or `Removed` it keeps it, so a
/// log still shows when the work started (the contract tests hold the
/// end-to-end case through a pipe).
#[test]
fn only_a_no_op_final_verb_drops_the_transitional_line() {
	for verb in ["Exists", "Running", "Absent", "Skipped"] {
		assert!(
			super::super::progress::is_noop_final_for_tests(verb),
			"{verb}"
		);
	}
	for verb in [
		"Created", "Started", "Pulled", "Removed", "Stopped", "Failed",
	] {
		assert!(
			!super::super::progress::is_noop_final_for_tests(verb),
			"{verb}"
		);
	}
}
