use super::{wants_interactive, ExecOptions};

/// #1079: `-T` opts out, matching `docker compose exec` — which has no `-i`
/// because a TTY on both ends is the default.
#[test]
fn no_tty_flag_disables_the_pty() {
	let opts = ExecOptions::default().with_no_tty_for_test(true);
	assert!(!wants_interactive(&opts, true));
}

/// `-d` detaches, so there is nobody to be interactive with.
#[test]
fn detach_disables_the_pty() {
	let opts = ExecOptions::default().with_detach_for_test(true);
	assert!(!wants_interactive(&opts, true));
}

/// The decisive one for existing users: with stdin not a terminal — any
/// script or pipeline — `exec` stays on the unchanged streaming path.
/// Allocating a pty there would change output framing for every script that
/// already calls `podup exec`.
#[test]
fn a_non_terminal_stdin_stays_on_the_streaming_path() {
	assert!(!wants_interactive(&ExecOptions::default(), false));
}

/// And the positive case, which the old ambient-stdin test could never
/// assert: defaults plus a terminal is what turns the pty on.
#[test]
fn a_terminal_stdin_with_defaults_is_interactive() {
	assert!(wants_interactive(&ExecOptions::default(), true));
}
