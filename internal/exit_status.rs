//! Mapping a failure onto a process exit status, and printing its reason.
//!
//! Split out of `main` because it is a closed responsibility with conventions
//! of its own to hold — docker's 126/127 for a command that cannot be launched,
//! 130 for an interrupt — and because `main` had reached the source line limit.

/// Map a failed launch onto docker's conventional exit codes by inspecting the
/// OCI/crun error text: a "command not found" failure → 127, a
/// "not executable"/"permission denied"/"exec format error" failure → 126,
/// anything else → 1. Pure string inspection so it is unit-testable.
/// Exit status for an attached `up` ended by a signal: 128 + SIGINT, the shell
/// convention, and what `docker compose up` returns for SIGTERM as well.
pub(crate) const fn interrupt_exit_code() -> i32 {
	130
}

pub(crate) fn command_failure_exit_code(msg: &str) -> i32 {
	let m = msg.to_ascii_lowercase();
	let not_found = m.contains("executable file not found")
		|| m.contains("not found in $path")
		|| (m.contains("oci runtime") && m.contains("no such file"));
	let not_executable = m.contains("exec format error")
		|| m.contains("not executable")
		|| (m.contains("oci runtime") && m.contains("permission denied"));
	if not_found {
		127
	} else if not_executable {
		126
	} else {
		1
	}
}

/// Print a top-level error to stderr with a colour-aware bold-red `error:` label.
/// anstream strips the styling when stderr is not a terminal or colour is off.
pub(crate) fn print_error(e: &podup::ComposeError) {
	use std::io::Write;
	let style = podup::ui::error_style();
	let mut err = anstream::stderr();
	let _ = writeln!(
		err,
		"podup: {}error:{} {e}",
		style.render(),
		style.render_reset()
	);
}

#[cfg(test)]
#[path = "exit_status_exit_code_tests.rs"]
mod exit_code_tests;
