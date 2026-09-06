use super::{wants_interactive_run, wants_interactive_with};

/// `-T` opts out, matching `docker compose run`, which has no `-i` because
/// a TTY on both ends is the default.
#[test]
fn no_tty_disables_the_pty() {
	assert!(!wants_interactive_run(true, false));
}

/// `-d` detaches, so there is nobody to be interactive with.
#[test]
fn detach_disables_the_pty() {
	assert!(!wants_interactive_run(false, true));
}

/// The decisive one for existing users: in a test harness, as in any script
/// or pipeline, stdin is not a terminal, so `run` stays on the unchanged
/// streaming path. Allocating a pty there would change output framing for
/// every script that already calls `podup run`.
#[test]
fn a_non_terminal_stdin_stays_on_the_streaming_path() {
	assert!(!wants_interactive_run(false, false));
}

/// Both ends are required, and this is the case that made it necessary:
/// `podup run app cmd > out.txt` typed at a shell leaves stdin a terminal
/// while stdout is a file. Checking stdin alone allocated a pty, and a pty
/// merges stdout with stderr and writes CRLF, so the redirect silently
/// changed the bytes the file received. Verified against `docker compose`,
/// which keeps it clean.
#[test]
fn a_redirected_stdout_stays_on_the_streaming_path() {
	assert!(!wants_interactive_with(false, false, true, false));
	assert!(wants_interactive_with(false, false, true, true));
}
