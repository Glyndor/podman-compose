//! Detect whether the running binary is owned by a system package manager.
//!
//! Self-update replaces the executable in place, which would corrupt a package
//! manager's record of the installed file. When the running binary is tracked by
//! such a manager the caller refuses and redirects the user to it.

#[cfg(target_os = "linux")]
use std::path::Path;

use crate::ComposeError;

/// Name of the system package manager that owns the running binary, if any.
///
/// Only `dpkg`/`apt` is detected, on Linux. cargo-install layouts
/// (`~/.cargo/bin`, `/usr/local/bin`) are not owned by `dpkg` and update
/// normally. A path no package owns returns `None`.
#[cfg(target_os = "linux")]
pub fn managing_package_manager() -> Option<&'static str> {
	let exe = std::env::current_exe().ok()?;
	let path = std::fs::canonicalize(&exe).unwrap_or(exe);
	dpkg_owns(&path).then_some("apt")
}

/// Whether dpkg's database records `path` as belonging to an installed package.
///
/// The primary check is `dpkg-query -S <path>`. Crucially this must not *fail
/// open*: if the helper cannot be spawned (missing, not executable, or denied)
/// we do not fall back to scanning dpkg's on-disk file lists directly
/// (`/var/lib/dpkg/info/*.list`) — that directory is owned by another package
/// and we have no guarantee of its mode, ownership, or the validity of its
/// contents. Reading it opens a window where a tampered target file's path
/// could be planted inside the listing directory to make self-update refuse
/// on an unmanaged binary, or, more worryingly, where the listing could be
/// truncated or replaced to hide the path of an apt-owned binary we are about
/// to clobber. Until `dpkg-query` is gone entirely (which is itself the
/// unambiguous signal that this is not a Debian-managed host) we report
/// `false` and let the update proceed.
#[cfg(target_os = "linux")]
fn dpkg_owns(path: &Path) -> bool {
	let query = std::process::Command::new("dpkg-query")
		.arg("-S")
		.arg(path)
		.output();
	// dpkg-query ran to completion: trust its verdict (success == owned).
	// A non-zero exit (path not in the database) is the common case and is
	// the same as a deploy from a cargo install or `/usr/local/bin`: report
	// `false` and let the update proceed.
	match query {
		Ok(output) => output.status.success(),
		// dpkg-query is missing, not executable, or denied: this is not a
		// host dpkg controls, so report `false` rather than consulting a
		// directory we do not own. The previous fallback that read the
		// `.list` files directly was the surface that prompted #1360: any
		// ownership/mode assumption about `/var/lib/dpkg/info` is a
		// privilege-confusion hypothesis, not a fact.
		Err(_) => false,
	}
}

/// Non-Linux platforms have no supported package-manager-managed install yet.
#[cfg(not(target_os = "linux"))]
pub fn managing_package_manager() -> Option<&'static str> {
	None
}

/// Error returned when the running binary is managed by package manager `pm`.
pub fn package_managed_error(pm: &str) -> ComposeError {
	ComposeError::Update(format!(
		"this podup was installed by {pm}; update it with your package manager \
		 (e.g. `apt upgrade podup`) rather than `podup update`, which would break \
		 the package's record of the file"
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn package_managed_error_names_the_manager() {
		let e = package_managed_error("apt");
		match e {
			ComposeError::Update(msg) => {
				assert!(msg.contains("apt"));
				assert!(msg.contains("podup update"));
			}
			_ => panic!("expected an Update error"),
		}
	}

	#[test]
	fn test_binary_is_not_package_managed() {
		// The test runner binary lives under target/, which no package owns, so
		// detection must not false-positive and block updates for normal builds.
		assert_eq!(managing_package_manager(), None);
	}

	/// #1360 (L10): `dpkg-query` is the only source of truth for whether apt
	/// owns the running binary. The previous implementation fell back to
	/// reading `/var/lib/dpkg/info/*.list` directly when `dpkg-query` could
	/// not be spawned — a directory owned by another package, with no mode
	/// or ownership guarantees. The fix is fail-closed: when `dpkg-query` is
	/// unavailable, report `false` and skip the scan entirely. We exercise
	/// the `Err` arm by removing `dpkg-query` from PATH via
	/// [`temp_env::with_var`]; `Command::new` resolves through PATH, so an
	/// empty / nonexistent path guarantees the spawn fails.
	#[cfg(target_os = "linux")]
	#[test]
	fn dpkg_owns_returns_false_when_dpkg_query_is_missing_from_path() {
		// /nonexistent is a directory that does not exist, so PATH cannot
		// resolve `dpkg-query` from it. The previous code would have
		// consulted the real `/var/lib/dpkg/info` directory (if present)
		// and might have answered `true` for a target whose path happened
		// to match. We pin the new behaviour: with `dpkg-query` unavailable,
		// the answer is unconditionally `false`.
		let empty_path = std::path::PathBuf::from("/nonexistent-empty-path-for-dpkg-test");
		let fake_target = Path::new("/usr/bin/podup");
		temp_env::with_var("PATH", Some(empty_path.display().to_string()), || {
			assert!(!dpkg_owns(fake_target));
		});
	}
}
