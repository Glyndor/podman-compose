use super::ensure_private_dir;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[test]
fn creates_fresh_private_dir() {
	let root = tempfile::tempdir().expect("tempdir");
	let dir = root.path().join("base");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };
	ensure_private_dir(&dir, euid).expect("fresh dir");
	let meta = std::fs::metadata(&dir).expect("metadata");
	assert_eq!(meta.mode() & 0o777, 0o700);
}

#[test]
fn drifted_permissions_on_existing_dir_fail_closed() {
	// A pre-existing directory with looser-than-0700 permissions is rejected
	// rather than chmod-healed: healing through the path would be a TOCTOU
	// window, and failing closed never writes secrets under a dir another
	// user may currently access.
	let root = tempfile::tempdir().expect("tempdir");
	let dir = root.path().join("base");
	std::fs::create_dir(&dir).expect("mkdir");
	std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };
	assert!(ensure_private_dir(&dir, euid).is_err());
	// Permissions are left untouched (no chmod attempted).
	let meta = std::fs::metadata(&dir).expect("metadata");
	assert_eq!(meta.mode() & 0o777, 0o755);
}

#[test]
fn symlinked_dir_is_rejected_not_healed() {
	let root = tempfile::tempdir().expect("tempdir");
	let target = root.path().join("real");
	let link = root.path().join("link");
	std::fs::create_dir(&target).expect("mkdir");
	std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).expect("chmod");
	std::os::unix::fs::symlink(&target, &link).expect("symlink");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };
	assert!(ensure_private_dir(&link, euid).is_err());
	// Target permissions stay untouched: no chmod through the link.
	let meta = std::fs::metadata(&target).expect("metadata");
	assert_eq!(meta.mode() & 0o777, 0o755);
}
