//! Terminal raw mode, window size and resize events, for interactive `exec`
//! and `run`.
//!
//! One contract, two implementations. Unix speaks termios, the `TIOCGWINSZ`
//! ioctl and `SIGWINCH`; Windows speaks the console API (`SetConsoleMode`,
//! `GetConsoleScreenBufferInfo`) and polls for window changes, because the
//! console has no resize signal to subscribe to. Callers see the same three
//! names either way — [`RawMode`], [`window_size`], [`ResizeWatcher`] — and
//! the pump in `terminal_pump` compiles once against them.
//!
//! The invariant both implementations exist to hold: **the terminal is
//! restored no matter how the exec ends.** A command that exits, a socket that
//! dies, a panic, or a `?` on some unrelated error must all leave the user's
//! shell usable. Raw mode with no echo and no line discipline is not something
//! to leave behind on an error path, so the restore lives in `Drop` rather
//! than at the end of the happy path.

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub(crate) use unix::{drain_stdin, window_size, RawMode, ResizeWatcher};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{drain_stdin, window_size, RawMode, ResizeWatcher};
