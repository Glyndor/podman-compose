//! Times one command for the benchmark harness.
//!
//! Prints a single line (`wall_s max_rss_kb cpu_s rc`) which `bench/run.sh`
//! appends to `raw.csv` in that field order.
//!
//! This replaces `/usr/bin/time -v`, whose `Elapsed` field is `h:mm:ss.SS`. At
//! hundredths of a second the two fastest rows in the suite (`running-ops ps`
//! and `config-heavy config`, both under 10 ms) published as `0.000`, while
//! `raw.csv` stored them as `%.6f` seconds, precision the instrument did not
//! have.
//!
//! Everything else about the measurement is unchanged. `ru_maxrss` is already
//! KB on Linux, CPU is `ru_utime + ru_stime`, and a waited-for child's rusage
//! folds in its own waited-for descendants, so a tool that shells out to
//! `podman` per call is still charged for that work.
//!
//! # Why this is a compiled binary and not a script
//!
//! `ru_maxrss` is not reset across `execve`: the child inherits its parent's
//! high-water mark, so the wrapper's own footprint becomes a floor under every
//! memory figure it reports. Measured on `/bin/true`, where the true cost is
//! about 1.3 MB:
//!
//! ```text
//! /usr/bin/time -v          1336 KB
//! this binary               1312 KB
//! python3 fork + execvp     6304 KB
//! python3 posix_spawnp      9792 KB
//! ```
//!
//! A Python wrapper puts a ~6 MB floor under a column that reports podup at
//! about 8.9 MB, which would leave that column measuring the wrapper. Writing
//! to `/proc/self/clear_refs` in the child before the exec does not help: the
//! child's resident set at that moment is still the interpreter's.
//!
//! The same run showed the interpreter inflating the clock on short commands:
//! `/bin/true` timed at 1.0 ms through `posix_spawnp` against 0.50 ms here.
//!
//! Usage: `timeit COMMAND [ARG...]`

use std::ffi::CString;
use std::process::ExitCode;
use std::time::Instant;

/// Exit code a shell reports for a command it could not execute.
const EXIT_NOT_EXECUTABLE: u8 = 127;

/// Turns the arguments into the NUL-terminated vector `execvp` expects.
///
/// Returns `None` if any argument contains an interior NUL, which no real
/// compose invocation does but which must not be passed on silently.
fn to_c_argv(args: &[String]) -> Option<Vec<CString>> {
	args.iter()
		.map(|a| CString::new(a.as_str()).ok())
		.collect()
}

/// Runs `argv` with stdout and stderr discarded, waits for it, and returns
/// `(wall_seconds, rusage, exit_code)`.
///
/// Output is dropped the way the harness dropped it when the command was
/// wrapped in `/usr/bin/time` and only the timing report was kept.
///
/// # Safety
///
/// Between `fork` and `execvp` the child calls only async-signal-safe
/// functions; every allocation it needs is made in the parent beforehand.
fn run(argv: &[CString]) -> (f64, libc::rusage, i32) {
	let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
	ptrs.push(std::ptr::null());
	let devnull = CString::new("/dev/null").expect("literal has no NUL");

	let start = Instant::now();
	unsafe {
		let pid = libc::fork();
		if pid == 0 {
			let fd = libc::open(devnull.as_ptr(), libc::O_WRONLY);
			if fd >= 0 {
				libc::dup2(fd, libc::STDOUT_FILENO);
				libc::dup2(fd, libc::STDERR_FILENO);
			}
			libc::execvp(ptrs[0], ptrs.as_ptr());
			// Only reached when the exec failed.
			libc::_exit(EXIT_NOT_EXECUTABLE as libc::c_int);
		}

		let mut status: libc::c_int = 0;
		let mut usage: libc::rusage = std::mem::zeroed();
		if pid < 0 || libc::wait4(pid, &mut status, 0, &mut usage) < 0 {
			return (start.elapsed().as_secs_f64(), usage, EXIT_NOT_EXECUTABLE as i32);
		}
		let wall = start.elapsed().as_secs_f64();

		// A signalled child is reported the way a shell reports it, so nothing
		// downstream has to decode a wait status.
		let code = if libc::WIFEXITED(status) {
			libc::WEXITSTATUS(status)
		} else {
			128 + libc::WTERMSIG(status)
		};
		(wall, usage, code)
	}
}

/// Seconds held in a `timeval`, as one float.
fn seconds(tv: libc::timeval) -> f64 {
	tv.tv_sec as f64 + tv.tv_usec as f64 / 1e6
}

fn main() -> ExitCode {
	let args: Vec<String> = std::env::args().skip(1).collect();
	if args.is_empty() {
		eprintln!("usage: timeit COMMAND [ARG...]");
		return ExitCode::from(2);
	}
	let Some(argv) = to_c_argv(&args) else {
		eprintln!("timeit: argument contains a NUL byte");
		return ExitCode::from(2);
	};

	let (wall, usage, code) = run(&argv);
	let cpu = seconds(usage.ru_utime) + seconds(usage.ru_stime);
	println!("{wall:.6} {} {cpu:.6} {code}", usage.ru_maxrss);

	// The harness reads the printed row, not this status; exiting non-zero on a
	// failed command would abort run.sh under a future `set -e`. Failed rows are
	// carried in the `rc` field and dropped by aggregate.py.
	ExitCode::SUCCESS
}
