//! The `generate quadlet` command: turn the compose file into Quadlet units and
//! either write them to a directory or print them to stdout.

use std::io::Write;
use std::path::Path;

/// Quadlet units are systemd unit files; they only run on Linux hosts (where
/// systemd consumes them from `~/.config/containers/systemd/`). Generating them
/// on macOS/Windows is legitimate (e.g. to deploy to a remote Linux host), so
/// this returns an advisory string rather than blocking. `os` is
/// [`std::env::consts::OS`]; the function is pure so every platform's branch is
/// testable in a single run.
fn quadlet_platform_advisory(os: &str) -> Option<String> {
	(os != "linux").then(|| {
		"quadlet units require systemd (Linux); generated files will not run on this host"
			.to_string()
	})
}

/// Write to stdout, treating a closed pipe (e.g. `podup generate quadlet | head`)
/// as a clean exit instead of a panic. With `panic = "abort"` the panic from a
/// raw `println!` on `EPIPE` aborts the process (exit 134) with a spurious
/// "internal error" message; a Unix tool should just stop quietly.
fn write_stdout(buf: &str) -> podup::Result<()> {
	match std::io::stdout().write_all(buf.as_bytes()) {
		Ok(()) => Ok(()),
		Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
		Err(e) => Err(e.into()),
	}
}

/// Generate Quadlet units from the compose file and either write them to a
/// directory or print them to stdout. Warnings about unmapped fields go to
/// stderr so stdout stays clean for piping.
pub(crate) fn write_quadlet(
	file: &podup::compose::types::ComposeFile,
	project: &str,
	base_dir: &Path,
	output: Option<&Path>,
	no_warn: bool,
) -> podup::Result<()> {
	// Reject configs the running commands would reject, before emitting anything.
	podup::quadlet::validate_for_quadlet(file)?;

	// The Quadlet path's host-binding / privilege-escalation warnings are
	// gated on `--no-warn` (issue #1358). The flag lives on a thread-local
	// because `generate_at` is a public free function and adding a parameter
	// would be a breaking change for downstream callers (helmly-agent is one).
	// The Quadlet command runs synchronously on the CLI thread, so a
	// thread-local has the same observable behaviour as a parameter would.
	let _guard = if no_warn {
		Some(podup::quadlet::NoWarnGuard::new())
	} else {
		None
	};

	let result = podup::quadlet::generate_at(file, project, base_dir);
	if let Some(dup) = result.duplicate_filename() {
		return Err(std::io::Error::new(
			std::io::ErrorKind::InvalidInput,
			format!(
				"quadlet: two resources map to the same unit file {dup:?}; \
				 rename one so their names do not collide after sanitization"
			),
		)
		.into());
	}
	if let Some(advisory) = quadlet_platform_advisory(std::env::consts::OS) {
		tracing::warn!("{advisory}");
	}
	for warning in &result.warnings {
		tracing::warn!("{warning}");
	}
	match output {
		Some(dir) => {
			let mut progress = String::new();
			for path in podup::quadlet::write_units(dir, &result.units)? {
				progress.push_str(&format!("wrote {}\n", path.display()));
			}
			write_stdout(&progress)?;
		}
		None => {
			let mut out = String::new();
			for unit in &result.units {
				out.push_str(&format!("# {}\n", unit.filename));
				out.push_str(&unit.contents);
				out.push('\n');
			}
			write_stdout(&out)?;
		}
	}
	Ok(())
}

#[cfg(test)]
#[path = "generate_tests.rs"]
mod tests;
