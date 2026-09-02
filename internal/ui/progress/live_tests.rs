use super::*;

/// The interrupt handler is claimed once per process, however many regions a
/// run opens. Two in one invocation — a `stats` after an `up` inside an
/// embedding crate — must not stack handlers, since each would race to call
/// `process::exit`.
///
/// The only test in the crate that may call this: the latch is
/// process-global, so a second caller would see whatever this one left.
#[test]
fn the_interrupt_handler_is_claimed_once() {
	assert!(claim_install(), "the first caller installs");
	assert!(!claim_install(), "the second must not");
	assert!(!claim_install());
}

/// The four sequences are what the repaint arithmetic is built on, so they
/// are pinned rather than left to a typo. `\x1b[3A` moves up three; `\x1b[J`
/// erases from the cursor down.
#[test]
fn the_cursor_moves_are_what_they_claim() {
	assert_eq!(cursor_up(3), "\u{1b}[3A");
	assert_eq!(CLEAR_BELOW, "\u{1b}[J");
	assert_eq!(HIDE_CURSOR, "\u{1b}[?25l");
	assert_eq!(SHOW_CURSOR, "\u{1b}[?25h");
}
