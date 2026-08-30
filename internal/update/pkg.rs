//! Detect whether the running binary is owned by a system package manager.
//!
//! Self-update replaces the executable in place, which would corrupt a package
//! manager's record of the installed file. When the running binary is tracked by
//! such a manager the caller refuses and redirects the user to it.

use std::path::Path;

use crate::ComposeError;

/// Name of the package manager that owns the running binary, if any.
///
/// Three are detected: apt on Linux, Homebrew on macOS and Linux, and Scoop on
/// Windows. cargo-install layouts (`~/.cargo/bin`, `/usr/local/bin`) belong to
/// none of them and update normally, and a path no manager owns returns `None`.
///
/// apt is asked; the other two are read off the path. That asymmetry is not
/// laziness. `dpkg-query` is a real database lookup with an authoritative
/// answer, while `brew` costs a process spawn on every `podup update` to learn
/// a prefix that is already visible in the path, and Scoop is a PowerShell
/// function rather than an executable, so there is nothing to spawn at all.
///
/// Both path checks can be wrong in one direction only, which is the direction
/// that matters. A layout that merely looks like Homebrew's or Scoop's makes
/// podup refuse to self-update and send the user to a package manager that does
/// not own it — wrong, visible, and recoverable by hand. The opposite mistake,
/// missing a managed install, silently rewrites a file whose manager still
/// believes it knows the contents. These heuristics are shaped to fail the
/// first way.
pub fn managing_package_manager() -> Option<&'static str> {
	let exe = std::env::current_exe().ok()?;
	let path = std::fs::canonicalize(&exe).unwrap_or(exe);

	#[cfg(target_os = "linux")]
	if dpkg_owns(&path) {
		return Some("apt");
	}
	if homebrew_owns(&path) {
		return Some("Homebrew");
	}
	if scoop_owns(&path) {
		return Some("Scoop");
	}
	None
}

/// Whether `path` sits inside a Homebrew Cellar.
///
/// Homebrew installs every formula under `<prefix>/Cellar/<name>/<version>/`
/// and links a symlink into `<prefix>/bin`, so the caller's `canonicalize`
/// resolves into the Cellar whichever of the two the user invoked. The prefix
/// itself moves — `/opt/homebrew` on Apple silicon, `/usr/local` on Intel,
/// `/home/linuxbrew/.linuxbrew` on Linux, anywhere at all for a custom install
/// — but the `Cellar` component does not, which is what makes it the thing to
/// match rather than any of the prefixes.
fn homebrew_owns(path: &Path) -> bool {
	path.components()
		.any(|c| c.as_os_str().eq_ignore_ascii_case("Cellar"))
}

/// Whether `path` sits inside a Scoop installation.
///
/// Scoop puts apps in `<root>/apps/<name>/<version>/` with shims in
/// `<root>/shims`. The shim is a launcher that starts the real executable, so
/// `current_exe` is the one under `apps` either way. The root defaults to
/// `%USERPROFILE%\scoop` and is moved with the `SCOOP` environment variable,
/// so that is consulted first; the `scoop`-then-`apps` pair is the fallback for
/// a root the variable does not name, such as a machine-wide `SCOOP_GLOBAL`
/// install another user configured.
fn scoop_owns(path: &Path) -> bool {
	scoop_owns_under(path, std::env::var_os("SCOOP").as_deref().map(Path::new))
}

/// The body of [`scoop_owns`] with the root passed in rather than read from the
/// environment, so the configured-root branch can be tested without mutating
/// `SCOOP` — which in a parallel test binary is a race against every other test
/// in the process rather than a fixture.
fn scoop_owns_under(path: &Path, root: Option<&Path>) -> bool {
	if let Some(root) = root {
		if path.starts_with(root.join("apps")) || path.starts_with(root.join("shims")) {
			return true;
		}
	}
	let parts: Vec<_> = path
		.components()
		.map(|c| c.as_os_str().to_owned())
		.collect();
	parts.windows(2).any(|w| {
		w[0].eq_ignore_ascii_case("scoop")
			&& (w[1].eq_ignore_ascii_case("apps") || w[1].eq_ignore_ascii_case("shims"))
	})
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

/// Error returned when the running binary is managed by package manager `pm`.
pub fn package_managed_error(pm: &str) -> ComposeError {
	// The example command used to be `apt upgrade podup` unconditionally, which
	// was correct while apt was the only manager detected and is a wrong
	// instruction the moment it is not.
	let how = match pm {
		"Homebrew" => "brew upgrade podup",
		"Scoop" => "scoop update podup",
		_ => "apt upgrade podup",
	};
	ComposeError::Update(format!(
		"this podup was installed by {pm}; update it with your package manager \
		 (`{how}`) rather than `podup update`, which would break the package's \
		 record of the file"
	))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Paths here use `/` even for the Windows layouts. `Path::components` splits
	/// on `\\` as well when it runs on Windows, so the component sequence these
	/// assert on is the one the real code sees there; a fixture written with
	/// `\\` would simply not parse into components on Linux and the tests would
	/// pass by asserting nothing.
	#[test]
	fn homebrew_layouts_are_recognised_on_every_prefix() {
		for p in [
			"/opt/homebrew/Cellar/podup/5.4.0/bin/podup",
			"/usr/local/Cellar/podup/5.4.0/bin/podup",
			"/home/linuxbrew/.linuxbrew/Cellar/podup/5.4.0/bin/podup",
			"/somewhere/entirely/custom/Cellar/podup/5.4.0/bin/podup",
		] {
			assert!(homebrew_owns(Path::new(p)), "not detected as Homebrew: {p}");
		}
	}

	#[test]
	fn ordinary_layouts_are_not_mistaken_for_homebrew() {
		for p in [
			"/usr/local/bin/podup",
			"/home/me/.cargo/bin/podup",
			"/usr/bin/podup",
			"/home/me/podup/target/release/podup",
		] {
			assert!(
				!homebrew_owns(Path::new(p)),
				"falsely detected as Homebrew: {p}"
			);
		}
	}

	#[test]
	fn scoop_layouts_are_recognised_without_the_variable() {
		for p in [
			"/c/Users/me/scoop/apps/podup/5.4.0/podup.exe",
			"/c/Users/me/scoop/shims/podup.exe",
			"/c/ProgramData/scoop/apps/podup/current/podup.exe",
		] {
			assert!(
				scoop_owns_under(Path::new(p), None),
				"not detected as Scoop: {p}"
			);
		}
	}

	#[test]
	fn a_relocated_scoop_root_is_recognised_through_the_variable() {
		// The fallback cannot see this one: no component is named `scoop`.
		let exe = Path::new("/d/tools/apps/podup/5.4.0/podup.exe");
		assert!(
			!scoop_owns_under(exe, None),
			"the fallback should not match a root with no `scoop` component"
		);
		assert!(
			scoop_owns_under(exe, Some(Path::new("/d/tools"))),
			"a root named by SCOOP must be honoured"
		);
	}

	#[test]
	fn ordinary_layouts_are_not_mistaken_for_scoop() {
		for p in [
			"/c/Program Files/podup/podup.exe",
			"/home/me/.cargo/bin/podup",
			// `scoop` present but not followed by apps or shims: a checkout of
			// the bucket repository, not an install.
			"/home/me/src/scoop/bucket/podup.json",
		] {
			assert!(
				!scoop_owns_under(Path::new(p), None),
				"falsely detected as Scoop: {p}"
			);
		}
	}

	#[test]
	fn the_error_tells_each_manager_its_own_command() {
		for (pm, cmd) in [
			("apt", "apt upgrade podup"),
			("Homebrew", "brew upgrade podup"),
			("Scoop", "scoop update podup"),
		] {
			let ComposeError::Update(msg) = package_managed_error(pm) else {
				panic!("expected an Update error");
			};
			assert!(
				msg.contains(pm),
				"{pm} is not named in its own error: {msg}"
			);
			assert!(
				msg.contains(cmd),
				"{pm} is told to run something other than {cmd}: {msg}"
			);
		}
	}

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
