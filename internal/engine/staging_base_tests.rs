//! `staging_base` decides where secret material is staged, and the decision
//! rests on `XDG_RUNTIME_DIR`: an absolute, private, own-user directory is used
//! (under `podup/`), anything else falls back to a per-uid directory under the
//! system temp dir. No test exercised that decision; a mutation sweep on
//! 2026-09-02 replaced the absolute-path guard with `true` and with `false`,
//! and the whole function with `Ok(Default::default())`, and every suite
//! stayed green.
//!
//! The environment is process-wide, so the cases take one lock and restore
//! the variable on every exit, including a panic.

use super::staging_base;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

static ENV: Mutex<()> = Mutex::new(());

/// Sets `XDG_RUNTIME_DIR` for the scope of the guard and puts the previous
/// value back on drop.
struct Xdg {
	_lock: MutexGuard<'static, ()>,
	previous: Option<std::ffi::OsString>,
}

impl Xdg {
	fn set(value: Option<&Path>) -> Self {
		let lock = ENV.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
		let previous = std::env::var_os("XDG_RUNTIME_DIR");
		// The process-wide environment is written under the lock above, which
		// every test in this module takes; nothing else in the unit suite reads
		// XDG_RUNTIME_DIR concurrently.
		match value {
			Some(p) => std::env::set_var("XDG_RUNTIME_DIR", p),
			None => std::env::remove_var("XDG_RUNTIME_DIR"),
		}
		Xdg {
			_lock: lock,
			previous,
		}
	}
}

impl Drop for Xdg {
	fn drop(&mut self) {
		match &self.previous {
			Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
			None => std::env::remove_var("XDG_RUNTIME_DIR"),
		}
	}
}

fn euid() -> u32 {
	// Read the owner of a directory this process just created rather than
	// calling geteuid here: the crate denies unsafe code outside the few
	// audited sites, and the effect is the same.
	use std::os::unix::fs::MetadataExt;
	let tmp = tempfile::tempdir().expect("tempdir");
	std::fs::metadata(tmp.path()).expect("metadata").uid()
}

fn private_dir(root: &Path, name: &str) -> PathBuf {
	use std::os::unix::fs::DirBuilderExt;
	let dir = root.join(name);
	std::fs::DirBuilder::new()
		.mode(0o700)
		.create(&dir)
		.expect("create private dir");
	dir
}

/// An absolute, private runtime dir is used, and the staging base is a
/// private directory inside it.
#[test]
fn an_absolute_private_xdg_runtime_dir_is_used() {
	use std::os::unix::fs::MetadataExt;
	let tmp = tempfile::tempdir().expect("tempdir");
	let xdg = private_dir(tmp.path(), "runtime");
	let _env = Xdg::set(Some(&xdg));

	let base = staging_base().expect("staging base");
	assert_eq!(base, xdg.join("podup"), "the base is XDG_RUNTIME_DIR/podup");
	let meta = std::fs::symlink_metadata(&base).expect("base exists");
	assert!(meta.is_dir());
	assert_eq!(meta.mode() & 0o777, 0o700, "created private");
	assert_eq!(meta.uid(), euid());
}

/// A relative `XDG_RUNTIME_DIR` is not a runtime dir; the fallback is the
/// per-uid directory under the temp dir, never something relative to the
/// working directory.
#[test]
fn a_relative_xdg_runtime_dir_falls_back_to_the_temp_dir() {
	let _env = Xdg::set(Some(Path::new("relative/runtime")));

	let base = staging_base().expect("staging base");
	assert_eq!(
		base,
		std::env::temp_dir().join(format!("podup-{}", euid())),
		"a relative value must not be honoured"
	);
	assert!(base.is_absolute());
}

/// Unset behaves like relative: the temp-dir fallback.
#[test]
fn an_unset_xdg_runtime_dir_falls_back_to_the_temp_dir() {
	let _env = Xdg::set(None);

	let base = staging_base().expect("staging base");
	assert_eq!(base, std::env::temp_dir().join(format!("podup-{}", euid())));
}

/// A runtime dir another user could write to is refused outright, with the
/// reason: secret staging under a world-writable path would be staging under
/// someone else's control.
#[test]
fn a_group_or_world_accessible_xdg_runtime_dir_is_refused() {
	use std::os::unix::fs::PermissionsExt;
	let tmp = tempfile::tempdir().expect("tempdir");
	let xdg = tmp.path().join("shared");
	std::fs::create_dir(&xdg).expect("create dir");
	std::fs::set_permissions(&xdg, std::fs::Permissions::from_mode(0o777)).expect("chmod");
	let _env = Xdg::set(Some(&xdg));

	let err = staging_base().expect_err("a shared runtime dir must be refused");
	let msg = err.to_string();
	assert!(
		msg.contains("not a private directory owned by the current user"),
		"refused for the right reason, not something else: {msg}"
	);
	assert!(
		!xdg.join("podup").exists(),
		"nothing is created under a directory that was refused"
	);
}
