//! Per-user private base directory used by the project lock.
//!
//! [`staging_base`] returns a directory that must never be usable by another
//! local user. On unix it is created 0700 under `$XDG_RUNTIME_DIR` (fallback:
//! `temp_dir()/podup-<euid>`) and verified — real directory, owned by the
//! current user, no group/other bits — failing closed on anything else. On
//! Windows the base lives under the per-user temp directory, whose default
//! ACLs already restrict access to the owning user; only the non-symlink
//! directory check applies. [`reject_dangerous_secret_mode`] guards a compose
//! `mode:` before it is applied to a native secret.

// libc FFI (geteuid) is needed here; the block carries a soundness comment.
// Opt back into `unsafe` for this module only.
#![allow(unsafe_code)]

use crate::error::{ComposeError, Result};
use std::path::PathBuf;

#[cfg(unix)]
use std::path::Path;

/// Whether `name` is safe to use as a single path component and container
/// name prefix. Matches docker-compose's project-name rule `^[a-z0-9][a-z0-9_-]*$`:
/// non-empty, bounded, lowercase ASCII letters/digits/`-`/`_` only, and a first
/// character that is a letter or digit. This rejects a leading separator
/// (`-rf`, `--all`, `_x` — a latent flag-injection vector for forwarding paths),
/// uppercase, dots (`.`, `..`, hidden directories, `bad.` trailing-dot names),
/// and all-separator names.
pub fn is_safe_project_name(name: &str) -> bool {
	if name.is_empty() || name.len() > 128 {
		return false;
	}
	let mut chars = name.chars();
	let first = chars.next().expect("name is non-empty");
	if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
		return false;
	}
	chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

/// Per-user staging base for inline secret/config content.
///
/// Prefers `$XDG_RUNTIME_DIR/podup` (per-user and 0700 by contract); falls
/// back to `temp_dir()/podup-<euid>`. The base sits in a world-writable
/// parent in the fallback case, so after creation it is verified to be a
/// real directory (not a symlink), owned by the current user, with no
/// group/other permission bits. Anything else aborts (fail closed) instead
/// of writing secret material under — or later deleting — a path another
/// local user may control.
#[cfg(unix)]
pub(super) fn staging_base() -> Result<PathBuf> {
	// SAFETY: geteuid takes no arguments, touches no memory and cannot fail.
	let euid = unsafe { libc::geteuid() };

	let base = match std::env::var_os("XDG_RUNTIME_DIR") {
		Some(dir) if Path::new(&dir).is_absolute() => {
			let xdg = PathBuf::from(&dir);
			// The runtime dir must itself be a private directory owned by us. A
			// hostile XDG_RUNTIME_DIR pointing at a shared/world-writable path
			// would otherwise host secret staging under another user's control.
			verify_private_dir(&xdg, euid)?;
			xdg.join("podup")
		}
		_ => std::env::temp_dir().join(format!("podup-{euid}")),
	};

	ensure_private_dir(&base, euid)?;
	Ok(base)
}

/// Per-user staging base on Windows: `%TEMP%\podup`.
///
/// Unlike `/tmp` on unix, the Windows temp directory resolves under the
/// user profile and its default ACLs grant access to the owning user only,
/// so no ownership or permission-bit verification applies — just the
/// non-symlink directory check.
#[cfg(windows)]
pub(super) fn staging_base() -> Result<PathBuf> {
	let base = std::env::temp_dir().join("podup");
	std::fs::create_dir_all(&base).map_err(ComposeError::Io)?;
	let meta = std::fs::symlink_metadata(&base).map_err(ComposeError::Io)?;
	if !meta.is_dir() || meta.file_type().is_symlink() {
		return Err(ComposeError::Unsupported(format!(
			"staging directory {} is not a private directory owned by the \
             current user — refusing to use it",
			base.display()
		)));
	}
	Ok(base)
}

/// Reject permission bits that are dangerous on a secret/config file no matter
/// where it is materialised: any execute bit (`0o111`) and the setuid/setgid/
/// sticky bits (`0o7000`). A secret/config holds data, never code, so these are
/// a misconfiguration or an attack and are refused unconditionally. `ctx` names
/// the offending secret/config in the error message.
///
/// This does **not** reject group/world-read bits: a Podman-native secret is
/// materialised inside the container's own mount namespace and `0o444` is the
/// Podman/compose default, so a readable mode is legitimate for that path.
pub(super) fn reject_dangerous_secret_mode(mode: u32, ctx: &str) -> Result<()> {
	if mode & 0o111 != 0 {
		return Err(ComposeError::Unsupported(format!(
			"mode {mode:#o} for {ctx} sets an execute bit on a secret/config; \
			 a secret holds data, never code (use e.g. 0o400 or 0o444)"
		)));
	}
	if mode & (0o4000 | 0o2000 | 0o1000) != 0 {
		return Err(ComposeError::Unsupported(format!(
			"mode {mode:#o} for {ctx} sets setuid, setgid, or sticky bits on a \
			 secret/config; these are refused (use e.g. 0o400 or 0o444)"
		)));
	}
	Ok(())
}

/// Create `dir` (0700) if needed and require it to be a private directory.
///
/// `DirBuilder` does not reset permissions on a pre-existing directory, so
/// a leftover directory we own whose bits drifted is self-healed with a
/// chmod first — only if it is a real directory (never chmod through a
/// symlink; in the worst race the chmod tightens something we own to 0700).
/// `verify_private_dir` then rejects anything not ours (fail closed).
#[cfg(unix)]
fn ensure_private_dir(dir: &Path, euid: u32) -> Result<()> {
	use std::os::unix::fs::DirBuilderExt;

	std::fs::DirBuilder::new()
		.recursive(true)
		.mode(0o700)
		.create(dir)
		.map_err(ComposeError::Io)?;

	// No chmod-healing of a pre-existing directory: re-applying the mode through
	// the path would be a TOCTOU window (a symlink swapped in between the stat
	// and the chmod). A freshly created directory is already 0700 from the mode
	// above; a pre-existing directory with the wrong owner or mode is rejected
	// by verify_private_dir below (fail closed).
	verify_private_dir(dir, euid)
}

/// Verify that `dir` is a non-symlink directory owned by `euid` with no
/// group/other permission bits.
#[cfg(unix)]
fn verify_private_dir(dir: &Path, euid: u32) -> Result<()> {
	use std::os::unix::fs::MetadataExt;

	let meta = std::fs::symlink_metadata(dir).map_err(ComposeError::Io)?;
	if !meta.is_dir() || meta.uid() != euid || meta.mode() & 0o077 != 0 {
		return Err(ComposeError::Unsupported(format!(
			"staging directory {} is not a private directory owned by the \
             current user — refusing to use it",
			dir.display()
		)));
	}
	Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "staging_name_tests.rs"]
mod name_tests;

#[cfg(all(test, unix))]
#[path = "staging_tests.rs"]
mod staging_tests;

#[cfg(all(test, unix))]
#[path = "staging_ensure_dir_tests.rs"]
mod ensure_dir_tests;

#[cfg(test)]
#[path = "staging_reject_mode_tests.rs"]
mod reject_mode_tests;

#[cfg(all(test, windows))]
#[path = "staging_windows_staging_tests.rs"]
mod windows_staging_tests;
