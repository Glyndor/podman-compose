//! Decide whether an exec-style command should allocate a pseudo-TTY.
//!
//! Pulled out of `engine::mod` so the engine module stays under the 500-line
//! hard cap enforced by the org's `line-limit` reusable. The decision is
//! small but tightly constrained: it has to match `docker compose run` exactly
//! (an interactive `run` only allocates a pty when stdin AND stdout are both
//! terminals — the famous redirect-test case, where the user pipes the
//! output to a file but leaves the keyboard attached), and the wrong answer
//! silently corrupts every script that already calls `podup run`.

/// Decide whether an interactive `run` should allocate a pseudo-TTY.
///
/// Both ends must be terminals. Passing `-T` (no_tty) or `-d` (detach) is
/// each sufficient on its own to opt out; a non-terminal stdin keeps `run`
/// on the streaming path even when stdout is a terminal (a redirect of an
/// interactive `run > out.txt` must keep the byte stream unchanged, not
/// merge it with stderr via a pty). Mirrors docker compose's matching rule.
pub(crate) fn wants_interactive_run(no_tty: bool, detach: bool) -> bool {
	wants_interactive_with(no_tty, detach, stdin_is_terminal(), stdout_is_terminal())
}

/// The four-way form, kept separate so the integration test on
/// `engine::mod::interactive_run_tests` can pin the redirect case
/// (`stdin=terminal, stdout=file`) without standing up the stream probes.
pub(crate) fn wants_interactive_with(
	no_tty: bool,
	detach: bool,
	stdin_tty: bool,
	stdout_tty: bool,
) -> bool {
	!no_tty && !detach && stdin_tty && stdout_tty
}

fn stdin_is_terminal() -> bool {
	use std::io::IsTerminal;
	std::io::stdin().is_terminal()
}

fn stdout_is_terminal() -> bool {
	use std::io::IsTerminal;
	std::io::stdout().is_terminal()
}

#[cfg(test)]
#[path = "interactive_tests.rs"]
mod tests;
