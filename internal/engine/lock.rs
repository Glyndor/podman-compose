//! Per-project advisory lock for mutating commands.
//!
//! Lifecycle commands (`up`, `down`, `start`, `stop`, `rm`, `kill`, `pause`,
//! `unpause`, `restart`, `run`, `build`) force-delete and recreate containers.
//! Two such commands running concurrently against the same project can
//! interleave their delete/create steps and race. [`Engine::lock_project`]
//! serializes them with a `flock(2)` advisory lock on a per-user, per-project
//! lock file; the lock is released when the returned guard is dropped.

// libc FFI (flock) is needed here; each block carries a soundness comment.
#![allow(unsafe_code)]

use crate::error::{ComposeError, Result};

use super::Engine;

/// Guard holding a held project lock. Dropping it releases the lock (the OS
/// drops the `flock` when the underlying file descriptor is closed).
#[must_use = "the project lock is released as soon as this guard is dropped"]
pub struct ProjectLock {
	#[allow(dead_code)]
	file: std::fs::File,
}

impl Engine {
	/// Acquire the exclusive project lock, blocking until it is available.
	///
	/// Held for the duration of a mutating command so concurrent `podup`
	/// processes operating on the same project cannot race container
	/// create/delete. On non-unix platforms this is a best-effort no-op
	/// (the lock file is created but not `flock`ed).
	pub fn lock_project(&self) -> Result<ProjectLock> {
		if !super::staging::is_safe_project_name(&self.project) {
			return Err(ComposeError::Unsupported(format!(
				"unsafe project name '{}' — refusing to create a lock file",
				self.project
			)));
		}
		let base = super::staging::staging_base()?;
		let path = base.join(format!("{}.lock", self.project));
		let file = open_lock_file(&path)?;
		acquire(&file)?;
		Ok(ProjectLock { file })
	}
}

#[cfg(unix)]
fn open_lock_file(path: &std::path::Path) -> Result<std::fs::File> {
	use std::os::unix::fs::OpenOptionsExt;
	std::fs::OpenOptions::new()
		.write(true)
		.create(true)
		.truncate(false)
		.mode(0o600)
		.open(path)
		.map_err(ComposeError::Io)
}

#[cfg(not(unix))]
fn open_lock_file(path: &std::path::Path) -> Result<std::fs::File> {
	std::fs::OpenOptions::new()
		.write(true)
		.create(true)
		.truncate(false)
		.open(path)
		.map_err(ComposeError::Io)
}

#[cfg(unix)]
fn acquire(file: &std::fs::File) -> Result<()> {
	use std::os::unix::io::AsRawFd;
	let fd = file.as_raw_fd();
	// SAFETY: `fd` is a valid, open file descriptor owned by `file` for the
	// duration of this call; flock only takes the fd and a flag and touches no
	// caller memory.
	let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
	if rc == 0 {
		return Ok(());
	}
	let err = std::io::Error::last_os_error();
	if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
		tracing::info!("waiting for project lock held by another podup process...");
		// SAFETY: same invariants as the non-blocking call above.
		let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
		if rc != 0 {
			return Err(ComposeError::Io(std::io::Error::last_os_error()));
		}
		return Ok(());
	}
	Err(ComposeError::Io(err))
}

#[cfg(not(unix))]
fn acquire(_file: &std::fs::File) -> Result<()> {
	Ok(())
}

#[cfg(all(test, unix))]
#[path = "lock_tests.rs"]
mod tests;
