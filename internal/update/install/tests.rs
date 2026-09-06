use super::*;
#[test]
fn platform_asset_matches_known_targets() {
	// Whatever host runs the tests, the asset (if any) must be one of the
	// release matrix names.
	if let Some(asset) = platform_asset() {
		assert!(asset.starts_with("podup-"));
	}
}
#[test]
fn platform_asset_covers_every_release_target() {
	// Pins the OS/ARCH → asset mapping to the full `release.yml` build
	// matrix so a newly added prebuilt (or a dropped arm) is caught here
	// instead of failing self-update silently in the field.
	let expected = [
		(("linux", "x86_64"), "podup-linux-x86_64"),
		(("linux", "aarch64"), "podup-linux-arm64"),
		(("macos", "aarch64"), "podup-darwin-arm64"),
		(("macos", "x86_64"), "podup-darwin-x86_64"),
		(("windows", "x86_64"), "podup-windows-x86_64.exe"),
		(("windows", "aarch64"), "podup-windows-arm64.exe"),
	];
	for ((os, arch), asset) in expected {
		assert_eq!(
			asset_for(os, arch),
			Some(asset),
			"self-update mapping drifted for {os}/{arch}"
		);
	}
}
#[test]
fn install_at_replaces_contents() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"old version").unwrap();
	install_at(&target, b"new version").unwrap();
	assert_eq!(std::fs::read(&target).unwrap(), b"new version");
}
/// A special bit on the target is never propagated onto the new binary.
///
/// `write_temp` copies the target's permissions so an install keeps whatever
/// mode the operator chose, and masks with `& 0o777` on the way. Without the
/// mask, a target that had been made setuid (by tampering, or by an
/// operator who did it on purpose once) would hand the freshly installed
/// podup the same bit, on a binary that has just been fetched over the
/// network. That is a privilege-escalation footgun, and it is the one
/// property of this function a test can actually observe.
///
/// The other three are window guards and cannot be reached in process: the
/// 0600 create mode is overwritten by this very copy before the function
/// returns, and `O_EXCL`/`O_NOFOLLOW` close a race between the unlink above
/// and the open. Their comments say so where they live.
#[cfg(unix)]
#[test]
fn write_temp_never_propagates_a_special_bit_from_the_target() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"old").unwrap();
	std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o4755)).unwrap();
	// Only meaningful if the filesystem kept the bit; some do not.
	let target_mode = std::fs::metadata(&target).unwrap().permissions().mode();
	if target_mode & 0o4000 == 0 {
		return;
	}
	let tmp = dir.path().join("podup.tmp");
	super::write_temp(&tmp, b"freshly downloaded", &target).unwrap();
	let mode = std::fs::metadata(&tmp).unwrap().permissions().mode();
	assert_eq!(
		mode & 0o7000,
		0,
		"a special bit rode from the target onto the new binary: {mode:o}"
	);
	assert_eq!(
		mode & 0o777,
		0o755,
		"the ordinary permission bits should still be carried over: {mode:o}"
	);
}
#[test]
fn install_at_creates_when_absent() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	install_at(&target, b"fresh").unwrap();
	assert_eq!(std::fs::read(&target).unwrap(), b"fresh");
}
#[cfg(unix)]
#[test]
fn install_at_preserves_executable_mode() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"old").unwrap();
	std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
	install_at(&target, b"new").unwrap();
	let mode = std::fs::metadata(&target).unwrap().permissions().mode();
	assert_eq!(mode & 0o777, 0o755);
}
#[cfg(unix)]
#[test]
fn install_at_strips_setuid_from_target_mode() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"old").unwrap();
	// A tampered/setuid target must not propagate its special bits onto the
	// freshly installed binary.
	std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o4755)).unwrap();
	install_at(&target, b"new").unwrap();
	let mode = std::fs::metadata(&target).unwrap().permissions().mode();
	assert_eq!(mode & 0o7000, 0, "setuid/setgid/sticky must be stripped");
	assert_eq!(mode & 0o777, 0o755);
}
#[test]
fn install_at_leaves_no_temp_files() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	install_at(&target, b"data").unwrap();
	let leftovers: Vec<_> = std::fs::read_dir(dir.path())
		.unwrap()
		.filter_map(|e| e.ok())
		.filter(|e| e.file_name().to_string_lossy().contains("update-"))
		.collect();
	assert!(leftovers.is_empty(), "temp file left behind");
}
#[test]
fn install_at_fails_when_target_dir_is_missing() {
	// A target whose parent directory does not exist must fail (the sibling
	// temp cannot be created) and must not leave anything behind.
	let dir = tempfile::tempdir().unwrap();
	let missing = dir.path().join("no-such-subdir");
	let target = missing.join("podup");
	assert!(install_at(&target, b"data").is_err());
	assert!(!missing.exists(), "must not create the missing parent dir");
}
/// #1360 (L5): the L5 swap window. The previous in-memory `Vec<u8>` backup
/// was one-shot: a kill between the swap and the self-test dropped the
/// in-memory copy and the user was left without a working binary. The
/// fix is `move_target_aside` before the swap: the `.old` sibling survives
/// any kill in the window, so the self-test has a recoverable copy on disk.
/// Simulate the kill by aborting the swap flow between the two renames and
/// confirm the `.old` is still there for the next run to roll back to.
#[test]
fn move_target_aside_leaves_the_old_binary_on_disk_for_rollback() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"the previous binary").unwrap();
	let backup = move_target_aside(&target).expect("the move-aside must succeed");
	assert!(
		backup.exists(),
		"the .old sibling must exist after the swap window, for the self-test to roll back to"
	);
	assert_eq!(
		std::fs::read(&backup).unwrap(),
		b"the previous binary",
		"the .old must hold the bytes that were at the target before the swap"
	);
	assert!(
		!target.exists(),
		"the target must be moved aside, not left in place"
	);
}
/// #1360 (L5): once the self-test passes, the `.old` sibling is no longer
/// needed and is reaped. `install_binary` does this directly, but the
/// behaviour here is the assertion that the helper functions are composed
/// correctly: `move_target_aside` + `restore_from_backup` round-trips the
/// original bytes through the `.old` path.
#[test]
fn restore_from_backup_round_trips_the_old_binary() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"the previous binary").unwrap();
	let backup = move_target_aside(&target).expect("the move-aside must succeed");
	// Pretend the swap installed a new binary. The rollback path is now
	// the user's safety net.
	std::fs::write(&target, b"the new binary (failing self-test)").unwrap();
	restore_from_backup(&target, &backup).expect("the rollback must succeed");
	assert_eq!(
		std::fs::read(&target).unwrap(),
		b"the previous binary",
		"the rollback must put the previous binary back at the target"
	);
	assert!(
		!backup.exists(),
		"the .old sibling is consumed by the rollback"
	);
}
/// #1360 (L5): a fresh-deploy install (no previous binary) leaves no
/// `.old` behind and the rollback path is a no-op. Simulates the
/// `install_binary` first-run on a machine that has never had podup.
#[test]
fn move_target_aside_is_a_no_op_when_the_target_does_not_exist() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	// No target to begin with, so the move-aside must still return a
	// backup path (the caller always has a place to roll back to / from).
	let backup = move_target_aside(&target).expect("the move-aside must succeed");
	assert!(
		!backup.exists(),
		"no .old is created when there was no target"
	);
	assert!(!target.exists(), "a non-existent target stays non-existent");
	// The rollback is a no-op too: there is nothing to roll back to.
	restore_from_backup(&target, &backup).expect("a no-op rollback must succeed");
}
/// #1360 (L5): `install_at` keeps the old in-place semantics, where the
/// target is replaced atomically. The L5 swap path adds a `move_target_aside`
/// *before* `install_at`, but the public surface (`install_at`) is the
/// building block tested here. The new `.old` sibling should not leak
/// to a directory that did not previously have one: the `.old` belongs
/// to the L5 caller, not to `install_at`.
#[test]
fn install_at_does_not_leave_an_old_sibling() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"old").unwrap();
	install_at(&target, b"new").unwrap();
	let old_sibling = dir.path().join("podup.old");
	assert!(
		!old_sibling.exists(),
		"install_at must not create a .old sibling; that is the L5 caller's job"
	);
}
/// #1367 (L5): the chmod-0000 case the issue's test plan calls out. A binary
/// the operator can no longer read or execute is the canonical "I have a
/// backup question for the rollback" case: the new bytes land on disk, the
/// self-test cannot even spawn the binary, and the L5 path must leave the
/// previous binary in place rather than leave the user with a half-installed
/// release. Simulate the full `install_binary` flow with a stub target: the
/// helper functions compose exactly the way the real function does, the
/// self-test fails for a chmod-0000 file (PermissionDenied at spawn), and
/// `restore_from_backup` puts the previous bytes back.
#[cfg(unix)]
#[test]
fn install_binary_rolls_back_when_the_target_is_unreadable() {
	use std::os::unix::fs::PermissionsExt;
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("podup");
	std::fs::write(&target, b"the previous binary").unwrap();

	let backup = move_target_aside(&target).expect("the move-aside must succeed");
	// The "new bytes" landing on disk: a real, fresh, chmod-0000 file the
	// kernel will refuse to exec, so the self-test cannot pass.
	std::fs::write(&target, b"the new binary").unwrap();
	std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o000)).unwrap();

	// Spawning a chmod-0000 file is a hard PermissionDenied: the self-test
	// must surface that as a failure, and the rollback must restore the
	// previous binary. Mirrors the real `install_binary` failure path.
	let err = self_test(&target, "9.9.9").expect_err("a chmod-0000 binary must fail its self-test");
	assert!(
		format!("{err}").contains("could not run"),
		"the error should be a spawn failure, got: {err}"
	);
	restore_from_backup(&target, &backup).expect("the rollback must succeed");
	assert_eq!(
		std::fs::read(&target).unwrap(),
		b"the previous binary",
		"the rollback must put the previous binary back at the target"
	);
	// The .old sibling is consumed by the rollback.
	assert!(
		!backup.exists(),
		"the .old sibling is consumed by the rollback"
	);
}
/// Write an executable stub script and return its path.
#[cfg(unix)]
fn write_stub(dir: &Path, name: &str, body: &str) -> PathBuf {
	use std::os::unix::fs::PermissionsExt;
	let p = dir.join(name);
	std::fs::write(&p, body).unwrap();
	std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
	p
}
#[cfg(unix)]
#[test]
fn self_test_passes_for_a_zero_exit_and_fails_otherwise() {
	let dir = tempfile::tempdir().unwrap();
	// A binary that exits 0 and reports the expected version passes; a
	// non-zero exit fails.
	let ok = write_stub(
		dir.path(),
		"ok",
		"#!/bin/sh\necho \"podup 9.9.9\"\nexit 0\n",
	);
	let bad = write_stub(dir.path(), "bad", "#!/bin/sh\nexit 1\n");
	assert!(self_test(&ok, "9.9.9").is_ok());
	assert!(self_test(&bad, "9.9.9").is_err());
	// A non-executable / missing target is a spawn error, not a panic.
	assert!(self_test(&dir.path().join("nope"), "9.9.9").is_err());
}
/// The classification that silently did not work.
///
/// A real executable held open for writing cannot be run (the kernel
/// returns ETXTBSY) so this produces the genuine errno rather than a
/// hand-built error, and asserts the predicate the retry depends on. The
/// previous version of this check ran on an error already formatted into a
/// `String`, where the errno no longer exists: it returned false every time,
/// the retry never fired, and the flake it was written to prevent stayed.
///
/// Linux only, deliberately. Whether a given kernel refuses to exec a file
/// held open for writing, and for a script whether the check lands on the
/// script or on its interpreter, is that kernel's business, verified on
/// Linux. Asserting it elsewhere tests the platform rather than the
/// classifier, and writing the test so it passes vacuously where ETXTBSY
/// never fires would repeat the mistake this whole change is about. The
/// retry itself stays on every Unix: it is a no-op where the errno does not
/// occur.
#[cfg(target_os = "linux")]
#[test]
fn a_binary_open_for_writing_is_classified_as_text_file_busy() {
	let dir = tempfile::tempdir().unwrap();
	let target = dir.path().join("held");
	std::fs::copy("/bin/sh", &target).expect("/bin/sh is copyable");
	{
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
	}
	// Held across the spawn on purpose; dropping it closes the window.
	let _writer = std::fs::OpenOptions::new()
		.write(true)
		.open(&target)
		.unwrap();
	let err = std::process::Command::new(&target)
		.arg("-c")
		.arg("exit 0")
		.spawn()
		.expect_err("a binary open for writing must not be executable");
	assert!(
		is_text_file_busy(&err),
		"ETXTBSY must be recognised from the io::Error itself, got {err:?}"
	);
}
#[cfg(unix)]
#[test]
fn self_test_rejects_a_version_mismatch() {
	let dir = tempfile::tempdir().unwrap();
	// A genuinely-signed but *older* replayed release exits 0 yet reports the
	// wrong version, so the rollback gate must reject it.
	let p = write_stub(
		dir.path(),
		"older",
		"#!/bin/sh\necho \"podup 1.0.0\"\nexit 0\n",
	);
	let err = self_test(&p, "9.9.9").unwrap_err();
	let msg = format!("{err}");
	assert!(msg.contains("rollback"), "{msg}");
	// A `v`-prefixed report still matches its unprefixed expectation.
	let v = write_stub(
		dir.path(),
		"vprefixed",
		"#!/bin/sh\necho \"podup v9.9.9\"\nexit 0\n",
	);
	assert!(self_test(&v, "9.9.9").is_ok());
}
#[test]
fn require_platform_asset_is_consistent() {
	match (platform_asset(), require_platform_asset()) {
		(Some(a), Ok(b)) => assert_eq!(a, b),
		(None, Err(_)) => {}
		_ => panic!("platform_asset and require_platform_asset disagree"),
	}
}
#[test]
fn rename_error_calls_out_permission_and_generic_cases() {
	let target = Path::new("/usr/local/bin/podup");
	// A permission error nudges the user toward elevation.
	let perm = rename_error(
		std::io::Error::from(std::io::ErrorKind::PermissionDenied),
		target,
	);
	match perm {
		ComposeError::Update(msg) => {
			assert!(msg.contains("permission denied"));
			assert!(msg.contains("sudo"));
		}
		_ => panic!("expected an Update error"),
	}
	// Any other error reports the underlying failure verbatim.
	let other = rename_error(std::io::Error::other("disk full"), target);
	match other {
		ComposeError::Update(msg) => {
			assert!(msg.contains("failed to install update"));
			assert!(msg.contains("disk full"));
		}
		_ => panic!("expected an Update error"),
	}
}
#[cfg(windows)]
#[test]
fn cleanup_stale_backup_removes_a_leftover_old_file() {
	// Simulates the case swap_into_place leaves behind: an `.old` sibling of
	// the running executable that its own best-effort delete could not
	// remove because the old process still held it open. The next updater
	// run calls this once nothing holds the file anymore, and it must go.
	let exe = std::env::current_exe().unwrap();
	let backup = exe.with_extension("old");
	std::fs::write(&backup, b"leftover backup").unwrap();
	cleanup_stale_backup();
	assert!(!backup.exists(), "the stale .old backup must be removed");
}
#[cfg(windows)]
#[test]
fn cleanup_stale_backup_is_a_no_op_without_a_leftover() {
	// No `.old` file present is the common case (a normal run, or a
	// platform that never took the Windows swap path) - must not error.
	let exe = std::env::current_exe().unwrap();
	let backup = exe.with_extension("old");
	let _ = std::fs::remove_file(&backup);
	cleanup_stale_backup();
	assert!(!backup.exists());
}
