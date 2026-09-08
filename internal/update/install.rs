//! Atomic, in-place replacement of the running binary.
//!
//! The verified new bytes are written to a temporary file in the *same*
//! directory as the target (so the final swap is a same-filesystem rename, which
//! is atomic) and then moved into place. On Unix the running binary's inode can
//! be replaced directly. On Windows a running `.exe` cannot be overwritten, so
//! the current file is renamed aside (`.old`) first. The immediate best-effort
//! delete of that backup can fail while the old process still holds the file
//! open; when it does, the leftover is removed at the start of the next
//! updater run (`cleanup_stale_backup`, called from [`crate::update::run`]),
//! not merely "the next run" of the binary in general.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::ComposeError;

/// The release asset name for the platform this binary was built for. Mirrors
/// the `release.yml` build matrix exactly.
pub fn platform_asset() -> Option<&'static str> {
	asset_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// Map an OS/ARCH pair to its release asset name. Split out from
/// [`platform_asset`] so the full matrix is testable without the host's values.
pub(crate) fn asset_for(os: &str, arch: &str) -> Option<&'static str> {
	match (os, arch) {
		("linux", "x86_64") => Some("podup-linux-x86_64"),
		("linux", "aarch64") => Some("podup-linux-arm64"),
		("macos", "aarch64") => Some("podup-darwin-arm64"),
		("macos", "x86_64") => Some("podup-darwin-x86_64"),
		("windows", "x86_64") => Some("podup-windows-x86_64.exe"),
		("windows", "aarch64") => Some("podup-windows-arm64.exe"),
		_ => None,
	}
}

/// Resolve the asset for the current platform or fail with a clear message.
pub fn require_platform_asset() -> crate::Result<&'static str> {
	platform_asset().ok_or_else(|| {
		ComposeError::Update(format!(
			"self-update is not supported on {}/{}; reinstall manually from \
			 https://github.com/Glyndor/podup/releases",
			std::env::consts::OS,
			std::env::consts::ARCH
		))
	})
}

/// Replace the currently running executable with `new_bytes`. Returns the path
/// that was updated. The caller MUST have verified `new_bytes` first.
///
/// `expected_version` is the resolved release version (no `v` prefix). The
/// self-test confirms the installed binary actually reports it: the signed
/// manifest binds asset bytes but not the release tag, so without this check a
/// man-in-the-middle able to spoof the release metadata could replay an older,
/// genuinely-signed release as the "latest" one (a rollback attack).
pub fn install_binary(new_bytes: &[u8], expected_version: &str) -> crate::Result<PathBuf> {
	let exe = std::env::current_exe()
		.map_err(|e| ComposeError::Update(format!("cannot locate current executable: {e}")))?;
	// Resolve symlinks so we replace the real file, not a symlink pointing at it.
	// Fail closed: replacing the symlink itself would orphan the real target.
	let target = std::fs::canonicalize(&exe).map_err(|e| {
		ComposeError::Update(format!(
			"cannot resolve the real path of {}: {e}",
			exe.display()
		))
	})?;
	// Move the current binary aside BEFORE the swap so a failed self-test
	// always has a rollback target on disk. The previous code read the
	// bytes into a `Vec<u8>` and re-ran the install on rollback, which is
	// a one-shot rollback: a kill between the swap and the self-test
	// drops the in-memory copy and the user is left without a working
	// binary. The on-disk `.old` sibling survives any kill in the window
	// and the self-test reads from it (which is the whole point of having
	// it on disk rather than in memory).
	let backup = move_target_aside(&target)?;
	if let Err(e) = install_at(&target, new_bytes) {
		// The swap itself failed: restore the old binary before reporting.
		let _ = restore_from_backup(&target, &backup);
		return Err(e);
	}
	if let Err(e) = self_test(&target, expected_version) {
		// Roll back to the original binary. The backup is on disk, so a
		// kill inside `restore_from_backup` would still leave the user
		// with a recoverable `.old` to fall back on.
		restore_from_backup(&target, &backup)?;
		return Err(ComposeError::Update(format!(
			"the updated binary failed its self-test ({e}); rolled back to the \
			 previous version"
		)));
	}
	// Best-effort remove the `.old`; a kill here just leaves a recoverable
	// sibling that the next run will tidy up.
	let _ = std::fs::remove_file(&backup);
	Ok(target)
}

/// Move the existing target to a sibling `.old` path so the swap and the
/// self-test can both find a recoverable copy of the previous binary. The
/// `.old` extension matches the Windows updater's path; the same name is
/// used on Unix so a human inspecting the install directory sees one
/// consistent leftover shape, and the same next-run cleanup applies.
pub(crate) fn move_target_aside(target: &Path) -> crate::Result<PathBuf> {
	let backup = target.with_extension("old");
	if backup == *target {
		return Err(ComposeError::Update(format!(
			"refusing to clobber the target with a sibling of the same path: {}",
			backup.display()
		)));
	}
	// Drop any stale leftover from a prior interrupted install. The
	// updater's next-run cleanup covers this too, but doing it here keeps
	// the rename atomic from the caller's perspective.
	if let Err(e) = std::fs::remove_file(&backup) {
		// ENOENT is fine; anything else is worth surfacing.
		if e.kind() != std::io::ErrorKind::NotFound {
			return Err(ComposeError::Update(format!(
				"cannot remove stale backup {}: {e}",
				backup.display()
			)));
		}
	}
	if target.exists() {
		std::fs::rename(target, &backup).map_err(|e| {
			ComposeError::Update(format!(
				"cannot move the current binary aside before the swap ({} -> {}): {e}",
				target.display(),
				backup.display()
			))
		})?;
	}
	Ok(backup)
}

/// Restore the `.old` sibling back onto `target`. Best-effort: the rollback
/// path reports the original error to the user regardless, but a failure
/// here is logged at debug so the next-run cleanup can pick up where this
/// left off (the `.old` is still on disk and the self-test has already
/// failed).
pub(crate) fn restore_from_backup(target: &Path, backup: &Path) -> crate::Result<()> {
	if !backup.exists() {
		// Nothing to restore from (the install was a fresh deploy onto a
		// path that did not exist before). The swap that already ran
		// stands.
		return Ok(());
	}
	std::fs::rename(backup, target).map_err(|e| {
		ComposeError::Update(format!(
			"failed to roll back to the previous binary from {}: {e}",
			backup.display()
		))
	})
}

/// How long to keep retrying a spawn that reports ETXTBSY.
///
/// The window is short by nature (it lasts only as long as some other process
/// holds a write descriptor to the file across its own exec) so a second is
/// generous. Bounded on purpose: a binary that is genuinely unrunnable must
/// reach the rollback, not wedge the updater retrying forever.
#[cfg(unix)]
const TEXT_FILE_BUSY_BUDGET: std::time::Duration = std::time::Duration::from_secs(1);

/// Spawn `target --version`, retrying while the kernel says the file is still
/// open for writing somewhere.
///
/// ETXTBSY is not a property of the binary: it means another process holds a
/// write descriptor to it across its own `exec`, and `O_CLOEXEC` does not close
/// that window. Treating it as a failed self-test rolls back a signed, verified,
/// perfectly good update over a race that resolves in milliseconds, and tells
/// the user their new version is broken.
pub(crate) fn spawn_version_probe(target: &Path) -> std::io::Result<std::process::Child> {
	use std::process::{Command, Stdio};

	let probe = || {
		Command::new(target)
			.arg("--version")
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()
	};

	#[cfg(unix)]
	{
		let deadline = std::time::Instant::now() + TEXT_FILE_BUSY_BUDGET;
		loop {
			match probe() {
				Err(e) if is_text_file_busy(&e) && std::time::Instant::now() < deadline => {
					std::thread::sleep(std::time::Duration::from_millis(10));
				}
				other => return other,
			}
		}
	}
	#[cfg(not(unix))]
	{
		probe()
	}
}

/// Whether a spawn error is ETXTBSY, asked of the `io::Error` itself.
///
/// This has to happen before the error is formatted into a message: once it is
/// a `String` the errno is gone, and matching on the text would break under any
/// locale or libc that words it differently.
#[cfg(unix)]
pub(crate) fn is_text_file_busy(e: &std::io::Error) -> bool {
	// `ExecutableFileBusy` is the named form; the raw errno is compared too so
	// this holds on a toolchain where the mapping differs.
	e.kind() == std::io::ErrorKind::ExecutableFileBusy || e.raw_os_error() == Some(libc::ETXTBSY)
}

/// Confirm a freshly-installed binary runs and reports `expected_version` by
/// invoking `--version`, bounded by a timeout so a hung binary can't wedge the
/// updater. The version check closes the rollback window: a replayed older
/// (signed) release fails here and is rolled back.
pub(crate) fn self_test(target: &Path, expected_version: &str) -> crate::Result<()> {
	use std::io::Read;
	use std::time::{Duration, Instant};

	let mut child = spawn_version_probe(target)
		.map_err(|e| ComposeError::Update(format!("could not run the updated binary: {e}")))?;

	let deadline = Instant::now() + Duration::from_secs(10);
	let status = loop {
		match child.try_wait() {
			Ok(Some(status)) => break status,
			Ok(None) => {
				if Instant::now() >= deadline {
					let _ = child.kill();
					return Err(ComposeError::Update(
						"updated binary did not respond to --version within 10s".to_string(),
					));
				}
				std::thread::sleep(Duration::from_millis(50));
			}
			Err(e) => {
				return Err(ComposeError::Update(format!(
					"waiting on the updated binary failed: {e}"
				)))
			}
		}
	};
	if !status.success() {
		return Err(ComposeError::Update(format!(
			"updated binary exited with {status} on --version"
		)));
	}
	// `--version` output is a single short line; reading after exit is safe
	// (it fits the pipe buffer, so the child never blocks on a full pipe).
	let mut out = String::new();
	if let Some(mut stdout) = child.stdout.take() {
		let _ = stdout.read_to_string(&mut out);
	}
	let reported_matches = out
		.split_whitespace()
		.any(|t| t == expected_version || t.trim_start_matches('v') == expected_version);
	if !reported_matches {
		return Err(ComposeError::Update(format!(
			"updated binary reports {:?} instead of the resolved release version \
			 {expected_version}; possible release-metadata tampering (rollback)",
			out.trim()
		)));
	}
	Ok(())
}

/// Write `new_bytes` to a sibling temp file and atomically move it onto
/// `target`, preserving the target's permissions. Factored out of
/// [`install_binary`] so the swap is testable against an arbitrary path.
pub fn install_at(target: &Path, new_bytes: &[u8]) -> crate::Result<()> {
	let dir = target.parent().ok_or_else(|| {
		ComposeError::Update(format!(
			"target {} has no parent directory",
			target.display()
		))
	})?;

	let file_name = target
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_else(|| "podup".to_string());
	let tmp = dir.join(format!(".{file_name}.update-{}", std::process::id()));

	write_temp(&tmp, new_bytes, target).inspect_err(|_| {
		let _ = std::fs::remove_file(&tmp);
	})?;

	if let Err(e) = swap_into_place(&tmp, target) {
		let _ = std::fs::remove_file(&tmp);
		return Err(e);
	}
	Ok(())
}

/// Write the new bytes to `tmp`, copy `target`'s permission bits (default 0755
/// on Unix when the target does not yet exist), and flush to disk.
pub(crate) fn write_temp(tmp: &Path, new_bytes: &[u8], target: &Path) -> crate::Result<()> {
	// Create the temp file private (0600) on Unix so the new binary's bytes are
	// never world-readable in a shared directory (e.g. /usr/local/bin) during the
	// window before the target's mode is applied. `File::create` honours the
	// process umask and could otherwise leave a 0644 file readable by other users.
	#[cfg(unix)]
	let mut f = {
		use std::os::unix::fs::OpenOptionsExt;
		// Remove any stale temp (e.g. from a crashed run); unlinking a symlink
		// removes the link itself and does not follow it.
		let _ = std::fs::remove_file(tmp);
		// `create_new` (O_EXCL) + O_NOFOLLOW: never follow or clobber a pre-planted
		// symlink in a shared/attacker-writable install directory, so the verified
		// bytes can only land in our own freshly created file.
		//
		// **Neither flag is reachable from a test, and that is a property of what
		// they guard rather than a gap.** The `remove_file` above already unlinks
		// any symlink planted beforehand (measured: after it, the link is gone
		// and its victim is untouched) so what is left for these flags is the
		// window *between* that unlink and this open. Closing a race is exactly
		// the thing an in-process test cannot enter. Mutations removing either
		// one survive the suite; the third property of this call, the 0600 mode,
		// is reachable and is pinned by
		// `write_temp_creates_the_file_private_to_this_user`.
		std::fs::OpenOptions::new()
			.write(true)
			.create_new(true)
			.custom_flags(libc::O_NOFOLLOW)
			.mode(0o600)
			.open(tmp)
			.map_err(|e| {
				ComposeError::Update(format!("cannot write update to {}: {e}", tmp.display()))
			})?
	};
	#[cfg(windows)]
	let mut f = {
		use std::os::windows::fs::OpenOptionsExt;
		// Mirror the Unix branch's invariants on Windows. `create_new(true)`
		// is the Windows analogue of `O_CREAT|O_EXCL`: after the unlink
		// below, the open fails if anything reappeared at `tmp` rather
		// than truncating it, so a
		// planted junction cannot redirect the verified bytes into a host
		// path the operator did not name. `custom_flags(0x0020_0000)` is
		// `FILE_FLAG_OPEN_REPARSE_POINT`: the open is given a handle to
		// the reparse point itself rather than the target it points at,
		// failing the call rather than silently traversing the link.
		// Together with the `remove_file` above (which unlinks a planted
		// symlink rather than following it) the verified bytes can only
		// land in a freshly created file at `tmp`. The constants are
		// intentionally inlined rather than imported from `windows-sys`
		// to keep this branch buildable on hosts that only have the
		// `Console` features wired up.
		let _ = std::fs::remove_file(tmp);
		std::fs::OpenOptions::new()
			.write(true)
			.create_new(true)
			.custom_flags(0x0020_0000)
			.open(tmp)
			.map_err(|e| {
				ComposeError::Update(format!("cannot write update to {}: {e}", tmp.display()))
			})?
	};
	#[cfg(not(any(unix, windows)))]
	let mut f = std::fs::File::create(tmp).map_err(|e| {
		ComposeError::Update(format!("cannot write update to {}: {e}", tmp.display()))
	})?;
	f.write_all(new_bytes).map_err(ComposeError::Io)?;
	f.flush().map_err(ComposeError::Io)?;

	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		// Copy the target's permission bits, but mask off setuid/setgid/sticky
		// (`& 0o777`): podup is an ordinary binary, and propagating a special bit
		// from a tampered target onto the freshly installed binary would be a
		// privilege-escalation footgun.
		let mode = std::fs::metadata(target)
			.map(|m| m.permissions().mode() & 0o777)
			.unwrap_or(0o755);
		std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode))
			.map_err(ComposeError::Io)?;
	}
	#[cfg(not(unix))]
	{
		let _ = target; // permissions are inherited on non-Unix.
	}

	f.sync_all().map_err(ComposeError::Io)?;
	Ok(())
}

/// Atomically move `tmp` onto `target`. Unix replaces the inode directly;
/// Windows renames the in-use file aside first.
#[cfg(not(windows))]
pub(crate) fn swap_into_place(tmp: &Path, target: &Path) -> crate::Result<()> {
	std::fs::rename(tmp, target).map_err(|e| rename_error(e, target))
}

#[cfg(windows)]
pub(crate) fn swap_into_place(tmp: &Path, target: &Path) -> crate::Result<()> {
	// A running .exe cannot be overwritten, but it can be renamed. Move it aside,
	// put the new binary in place, then best-effort delete the old one (it may
	// still be locked while running - if so, it is removed at the start of the
	// next updater run by `cleanup_stale_backup`).
	//
	// The L5 swap path
	// (`install_binary` → `move_target_aside` → `install_at`) means the target
	// may already be absent when this function is called: the caller has moved
	// it to `.old` so the self-test has a recoverable copy on disk. Track
	// whether *this* call created the backup so the cleanup at the end only
	// touches the file we created, and the rename error path only tries to
	// restore from a backup we own.
	let backup = target.with_extension("old");
	let target_existed = target.exists();
	if target_existed {
		// Drop any stale leftover before re-creating it. Only meaningful when
		// we are about to be the one to create a fresh `.old`.
		let _ = std::fs::remove_file(&backup);
		std::fs::rename(target, &backup).map_err(|e| rename_error(e, target))?;
	}
	if let Err(e) = std::fs::rename(tmp, target) {
		// Roll back so the user is not left without a binary, but only when
		// we created the backup in this call. A pre-existing `.old` (the L5
		// swap path) is the caller's responsibility to restore.
		if target_existed {
			let _ = std::fs::rename(&backup, target);
		}
		return Err(rename_error(e, target));
	}
	// Only clean up the backup if we created it in this call. A pre-existing
	// `.old` belongs to the caller, who decides when (and whether) to drop it.
	if target_existed {
		let _ = std::fs::remove_file(&backup);
	}
	Ok(())
}

/// Best-effort removal of a `.old` backup [`swap_into_place`] could not delete
/// immediately because the old process still held the file open. Call once at
/// the start of every updater run ([`crate::update::run`]): by then the
/// process that produced the backup has exited, so the file is no longer
/// locked and the leftover clears on this run rather than lingering until the
/// user happens to run another update. Silently does nothing if there is no
/// leftover, or if removal still fails for some other reason.
#[cfg(windows)]
pub(crate) fn cleanup_stale_backup() {
	if let Ok(exe) = std::env::current_exe() {
		let _ = std::fs::remove_file(exe.with_extension("old"));
	}
}

/// Turn a rename failure into an actionable error, calling out the common
/// permission case (system install dirs need elevation).
pub(crate) fn rename_error(e: std::io::Error, target: &Path) -> ComposeError {
	if e.kind() == std::io::ErrorKind::PermissionDenied {
		ComposeError::Update(format!(
			"permission denied writing {}; re-run with elevated privileges \
			 (e.g. sudo) or set a writable install location",
			target.display()
		))
	} else {
		ComposeError::Update(format!(
			"failed to install update to {}: {e}",
			target.display()
		))
	}
}

#[cfg(test)]
mod tests;
