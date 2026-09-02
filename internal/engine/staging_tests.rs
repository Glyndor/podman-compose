use super::verify_private_dir;
use std::os::unix::fs::PermissionsExt;

#[test]
fn private_dir_accepted() {
	let dir = tempfile::tempdir().expect("tempdir");
	std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };
	assert!(verify_private_dir(dir.path(), euid).is_ok());
}

#[test]
fn group_accessible_dir_rejected() {
	let dir = tempfile::tempdir().expect("tempdir");
	std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).expect("chmod");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };
	assert!(verify_private_dir(dir.path(), euid).is_err());
}

#[test]
fn foreign_owner_rejected() {
	let dir = tempfile::tempdir().expect("tempdir");
	std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).expect("chmod");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let other = unsafe { libc::geteuid() } + 1;
	assert!(verify_private_dir(dir.path(), other).is_err());
}

#[test]
fn symlink_rejected() {
	let dir = tempfile::tempdir().expect("tempdir");
	let target = dir.path().join("real");
	let link = dir.path().join("link");
	std::fs::create_dir(&target).expect("mkdir");
	std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).expect("chmod");
	std::os::unix::fs::symlink(&target, &link).expect("symlink");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };
	assert!(verify_private_dir(&link, euid).is_err());
}

#[test]
fn regular_file_rejected() {
	let dir = tempfile::tempdir().expect("tempdir");
	let file = dir.path().join("file");
	std::fs::write(&file, b"x").expect("write");
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };
	assert!(verify_private_dir(&file, euid).is_err());
}
