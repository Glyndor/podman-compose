//! The console-API implementation of the terminal contract ,  see the module
//! doc in `mod.rs` for the contract and the restore-on-drop invariant.
//!
//! Windows has no termios: raw mode is a console *mode* cleared on the stdin
//! handle (line input, echo, Ctrl-C processing) plus virtual-terminal flags on
//! both ends, so keystrokes arrive as VT byte sequences and the remote pty's
//! VT output renders instead of printing as garbage. And it has no `SIGWINCH`:
//! window changes are found by polling the screen-buffer size, which is cheap
//! enough at four times a second to be imperceptible and avoids competing with
//! the stdin pump for console input events.

// The console mode and screen-buffer calls are Win32 FFI. The crate denies
// `unsafe` and modules that need it opt back in locally, with a soundness
// comment per block ,  see `engine::lock` and `engine::staging` for the same
// pattern.
#![allow(unsafe_code)]

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
	GetConsoleMode, GetConsoleScreenBufferInfo, GetNumberOfConsoleInputEvents, GetStdHandle,
	ReadConsoleInputW, SetConsoleMode, CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT,
	ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_INPUT,
	ENABLE_VIRTUAL_TERMINAL_PROCESSING, INPUT_RECORD, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
	STD_OUTPUT_HANDLE,
};

/// A console switched to raw mode, restored on drop.
///
/// Holding the *original* modes rather than reconstructing "sane" ones
/// matters: the caller's shell may run with its own console flags, and putting
/// the console into what podup thinks is normal would quietly change them.
/// Both ends are touched ,  stdin so keystrokes stop being line-buffered and
/// echoed, stdout so the VT sequences a pty emits are interpreted ,  so both
/// originals are held and both are restored.
pub(crate) struct RawMode {
	stdin: HANDLE,
	stdin_original: CONSOLE_MODE,
	stdout: HANDLE,
	stdout_original: CONSOLE_MODE,
}

// SAFETY: the standard console handles are process-global pseudo-handles, not
// tied to the thread that retrieved them; `SetConsoleMode` is documented as
// callable from any thread. The raw pointers inside `HANDLE` are what stops
// the auto-impl, not any real thread affinity.
unsafe impl Send for RawMode {}

impl RawMode {
	/// Switch the console to raw mode, or return `None` when stdin or stdout
	/// is not a console.
	///
	/// Not being a console is the ordinary case for `podup exec` in a script
	/// or a pipeline, not an error: there is no line discipline to disable,
	/// and the caller streams bytes as before.
	pub(crate) fn enable() -> Option<Self> {
		// SAFETY: `GetStdHandle` takes no pointers and only returns a handle;
		// a missing standard handle comes back null or invalid, checked below.
		let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
		let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
		Self::enable_on(stdin, stdout)
	}

	/// The same, on explicit handles.
	///
	/// `enable` is the only place that consults the ambient handles, which
	/// keeps this testable: a test that asserted on `enable()` directly would
	/// be asserting on whether the test runner has a console ,  and on the way
	/// to failing it would put that console into raw mode.
	pub(super) fn enable_on(stdin: HANDLE, stdout: HANDLE) -> Option<Self> {
		if stdin.is_null() || stdin == INVALID_HANDLE_VALUE {
			return None;
		}
		if stdout.is_null() || stdout == INVALID_HANDLE_VALUE {
			return None;
		}

		let mut stdin_original: CONSOLE_MODE = 0;
		// SAFETY: `stdin_original` is a correctly sized mode owned here, and
		// `GetConsoleMode` only writes into it. A non-console handle fails the
		// call rather than misbehaving ,  that is exactly the "not a terminal"
		// answer.
		if unsafe { GetConsoleMode(stdin, &mut stdin_original) } == 0 {
			return None;
		}
		let mut stdout_original: CONSOLE_MODE = 0;
		// SAFETY: as above, for the output handle.
		if unsafe { GetConsoleMode(stdout, &mut stdout_original) } == 0 {
			return None;
		}

		// Line input and echo are the console's line discipline; processed
		// input turns Ctrl-C into an event instead of a byte. All three must
		// go so keystrokes ,  including Ctrl-C ,  travel to the remote command,
		// which is what raw mode means. Virtual-terminal input makes arrows
		// and function keys arrive as the VT sequences the remote pty expects.
		let stdin_raw = (stdin_original
			& !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
			| ENABLE_VIRTUAL_TERMINAL_INPUT;
		// SAFETY: the handle is a console (its mode was just read) and the
		// mode is a plain flag word derived from the current one.
		if unsafe { SetConsoleMode(stdin, stdin_raw) } == 0 {
			return None;
		}

		// Added to the current flags, never replacing them: anstream may have
		// already enabled VT processing for colour output, and clobbering the
		// rest of the mode word would undo whatever else the shell had set.
		let stdout_raw = stdout_original | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
		// SAFETY: as above, for the output handle.
		if unsafe { SetConsoleMode(stdout, stdout_raw) } == 0 {
			// stdin is already raw; put it back before reporting "no console"
			// so a failed enable never half-changes the caller's terminal.
			// SAFETY: restoring the exact mode read from this handle above.
			unsafe { SetConsoleMode(stdin, stdin_original) };
			return None;
		}

		Some(Self {
			stdin,
			stdin_original,
			stdout,
			stdout_original,
		})
	}
}

impl Drop for RawMode {
	fn drop(&mut self) {
		// SAFETY: these are the exact modes read from these same handles in
		// `enable_on`, so restoring them cannot put the console in a state it
		// was not already in. Errors are ignored because there is nothing
		// useful to do while unwinding, and reporting one would replace the
		// real error the caller is already handling.
		unsafe {
			SetConsoleMode(self.stdin, self.stdin_original);
			SetConsoleMode(self.stdout, self.stdout_original);
		}
	}
}

/// The console window's current size as `(rows, cols)`, or `None` when no
/// handle can answer.
///
/// Used to size the remote pty at start and to follow it on window changes:
/// without this, a full-screen program inside the container draws to an 80x24
/// default and redraws wrong the moment the window changes.
///
/// **stdout first, then stderr** ,  not stdin, unlike Unix: the screen buffer
/// is an output-side object, and the input handle does not answer
/// `GetConsoleScreenBufferInfo`. stderr covers a redirected stdout, the same
/// reverse case the Unix fallback covers.
pub(crate) fn window_size() -> Option<(u16, u16)> {
	// SAFETY: `GetStdHandle` takes no pointers; `size_of` validates what it
	// returns.
	let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
	let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
	size_of(stdout).or_else(|| size_of(stderr))
}

/// `GetConsoleScreenBufferInfo` on one handle.
fn size_of(handle: HANDLE) -> Option<(u16, u16)> {
	if handle.is_null() || handle == INVALID_HANDLE_VALUE {
		return None;
	}
	// SAFETY: `info` is a correctly sized, zeroed struct owned here, and the
	// call only writes into it. A non-console handle fails the call rather
	// than writing garbage.
	let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
	if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } != 0 {
		return window_extent(&info);
	}
	None
}

/// The *visible window* extent from a screen-buffer report, as `(rows, cols)`.
///
/// `srWindow`, not `dwSize`: the buffer runs thousands of lines of scrollback,
/// and sizing the remote pty to it would have a full-screen program paint a
/// frame taller than the screen. The rectangle is inclusive on both ends,
/// hence the `+ 1`s. Pure, so the arithmetic ,  the part worth pinning ,  is
/// testable without a console.
fn window_extent(info: &CONSOLE_SCREEN_BUFFER_INFO) -> Option<(u16, u16)> {
	let rows = i32::from(info.srWindow.Bottom) - i32::from(info.srWindow.Top) + 1;
	let cols = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
	// A degenerate or inverted rectangle has no usable geometry ,  treat it as
	// unknown rather than sizing the remote pty to nothing.
	if rows <= 0 || cols <= 0 {
		return None;
	}
	Some((rows as u16, cols as u16))
}

/// Resize events for the interactive pump: yields a size to apply whenever the
/// caller's window changes.
///
/// The console has no resize signal, so this polls [`window_size`] four times
/// a second and reports only changes. The alternative ,  `ReadConsoleInput`
/// watching `WINDOW_BUFFER_SIZE_EVENT` ,  is event-driven but competes with the
/// stdin byte pump for the same console handle; polling costs one cheap call
/// per tick and touches nothing the pump owns.
pub(crate) struct ResizeWatcher {
	interval: tokio::time::Interval,
	last: Option<(u16, u16)>,
}

impl ResizeWatcher {
	/// Start watching, from the current size.
	///
	/// `Result` for signature parity with the Unix watcher, whose signal
	/// registration can genuinely fail; this one cannot.
	pub(crate) fn new() -> std::io::Result<Self> {
		let mut interval = tokio::time::interval(std::time::Duration::from_millis(250));
		// A pump busy streaming output can miss ticks; catching up in a burst
		// would report the same size several times for no reason.
		interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
		Ok(Self {
			interval,
			last: window_size(),
		})
	}

	/// The next size to apply: pends until a poll observes a change.
	pub(crate) async fn next(&mut self) -> (u16, u16) {
		loop {
			self.interval.tick().await;
			if let Some(size) = resize_due(&mut self.last, window_size()) {
				return size;
			}
		}
	}
}

/// Whether a polled size warrants a resize: `Some` only when it is readable
/// and differs from the last one reported, recording it as reported. Pure so
/// the dedup ,  the part that decides whether the pump spams resize calls ,
/// is testable without a console.
fn resize_due(last: &mut Option<(u16, u16)>, current: Option<(u16, u16)>) -> Option<(u16, u16)> {
	let current = current?;
	if *last == Some(current) {
		return None;
	}
	*last = Some(current);
	Some(current)
}

/// Drop every byte the kernel had queued on the console's stdin handle, without
/// forwarding any of them.
///
/// Called once at the start of an interactive exec/run, between enabling raw
/// mode and entering the byte pump. The console's input queue may carry
/// startup bytes from before the user could possibly type anything, and a
/// previously-measured pty artifact echoes back as `^@` (see #1675).
/// Discarding those bytes before the pump starts keeps the user's first
/// keystroke from being preceded by a stray character.
///
/// A non-console handle is a no-op ,  the pump does not run on a pipe.
pub(crate) fn drain_stdin() {
	// SAFETY: `GetStdHandle` takes no pointers and only returns a handle; a
	// missing standard handle comes back null or invalid, checked below.
	let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
	if stdin.is_null() || stdin == INVALID_HANDLE_VALUE {
		return;
	}
	// SAFETY: `events` is a correctly sized unsigned owned here; the call only
	// writes into it. A non-console handle returns 0.
	let mut events: u32 = 0;
	if unsafe { GetNumberOfConsoleInputEvents(stdin, &mut events) } == 0 {
		return;
	}
	let mut records = vec![std::mem::zeroed::<INPUT_RECORD>(); events.max(1) as usize];
	let mut read: u32 = 0;
	// SAFETY: `records` is a contiguous, correctly-sized buffer; the call
	// only writes up to `events` records and reports the count in `read`.
	// We discard the records (they are bytes from before raw mode took
	// effect; the user has not had a chance to type anything yet).
	let _ = unsafe { ReadConsoleInputW(stdin, records.as_mut_ptr(), events, &mut read) };
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
