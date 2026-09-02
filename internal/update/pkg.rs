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

/// What this machine's unattended-upgrades configuration says about Glyndor.
#[derive(Debug, PartialEq)]
pub(crate) enum GlyndorAutoUpdate {
	/// Some rule permits the origin and nothing vetoes the package.
	Permitted,
	/// Nothing will ever install a Glyndor update here, with the reason.
	Blocked(&'static str),
	/// The question could not be answered, so nothing is said about it.
	Unknown,
}

/// Read the verdict out of `apt-config dump` output.
///
/// Pure so the three configurations that matter can be fed to it directly.
/// A check that has only ever been seen to agree with the configuration we
/// write ourselves has not been tested against the ones it meets in the field.
///
/// **Two lists, not one.** unattended-upgrades' own README documents
/// `Allowed-Origins` *or* `Origins-Pattern` as alternatives, and the keyring
/// happens to write the first. An operator using the second is covered, and a
/// check that knew only about the first would tell them nothing will ever
/// update them while their machine updates itself fine. That false alarm is the
/// noise this exists to avoid, so it would have been worse than saying nothing.
///
/// **And a third way to be stuck.** `Package-Blacklist` vetoes by name, so an
/// allowed origin is not sufficient on its own.
///
/// The origin match is deliberately loose — any entry mentioning `Glyndor`
/// counts, rather than `Glyndor:stable` exactly. An operator who allowed a
/// different Glyndor suite has thought about this and does not need telling;
/// the machine worth warning is the one whose configuration has never heard of
/// Glyndor at all.
pub(crate) fn glyndor_auto_update(apt_config_dump: &str) -> GlyndorAutoUpdate {
	let mut saw_any_rule = false;
	let mut permits_glyndor = false;
	let mut blacklists_podup = false;

	for line in apt_config_dump.lines() {
		let line = line.trim();
		let Some(rest) = line.strip_prefix("Unattended-Upgrade::") else {
			continue;
		};
		if rest.starts_with("Allowed-Origins") || rest.starts_with("Origins-Pattern") {
			saw_any_rule = true;
			if rest.contains("Glyndor") {
				permits_glyndor = true;
			}
		} else if rest.starts_with("Package-Blacklist") && rest.contains("podup") {
			blacklists_podup = true;
		}
	}

	if blacklists_podup {
		return GlyndorAutoUpdate::Blocked(
			"podup is in Unattended-Upgrade::Package-Blacklist, so unattended-upgrades \
			 will never install an update for it even though the origin is allowed",
		);
	}
	if permits_glyndor {
		return GlyndorAutoUpdate::Permitted;
	}
	// No rule of either kind was seen at all: unattended-upgrades is probably not
	// configured on this machine, or `apt-config` returned something this does not
	// understand. Either way the honest answer is that the question was not
	// answered, not that the machine is broken.
	if !saw_any_rule {
		return GlyndorAutoUpdate::Unknown;
	}
	GlyndorAutoUpdate::Blocked(
		"the Glyndor archive is in neither Unattended-Upgrade::Allowed-Origins nor \
		 Unattended-Upgrade::Origins-Pattern, so unattended-upgrades will never \
		 install a podup update. Installing glyndor-archive-keyring adds the rule; \
		 `apt upgrade` once is what pulls it in",
	)
}

/// Ask apt for its merged configuration. `None` when it cannot be asked.
fn apt_config_dump() -> Option<String> {
	let out = std::process::Command::new("apt-config")
		.arg("dump")
		.output()
		.ok()?;
	out.status
		.success()
		.then(|| String::from_utf8_lossy(&out.stdout).into_owned())
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
	// For apt only, and only here: the operator has just asked to update and is
	// being sent to a mechanism that, on some machines, will never run. Telling
	// them to use apt without saying that would be a correct instruction and a
	// misleading one. This is the single place it is worth the process spawn --
	// nothing on an ordinary command path pays for it.
	let also = if pm == "apt" {
		match apt_config_dump().as_deref().map(glyndor_auto_update) {
			Some(GlyndorAutoUpdate::Blocked(why)) => format!(". Note that {why}"),
			_ => String::new(),
		}
	} else {
		String::new()
	};
	ComposeError::Update(format!(
		"this podup was installed by {pm}; update it with your package manager \
		 (`{how}`) rather than `podup update`, which would break the package's \
		 record of the file{also}"
	))
}

#[cfg(test)]
#[path = "pkg_tests.rs"]
mod tests;
