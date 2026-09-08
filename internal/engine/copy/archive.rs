//! Tar packing and extraction for `cp`.
//!
//! The pure, container-free half of `cp`: it moves bytes between the host
//! filesystem and a tar stream and carries the security hardening: the
//! zip-slip guard, the extracted-mode sanitization and the docker/podman
//! destination semantics. Kept synchronous so the guards can be unit-tested
//! without a container.

use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::error::{ComposeError, Result};

pub(super) fn pack_path(
	src: &Path,
	follow_link: bool,
	name_override: Option<&str>,
) -> Result<Vec<u8>> {
	let encoder = GzEncoder::new(Vec::new(), Compression::default());
	let mut tar = tar::Builder::new(encoder);
	// `-L/--follow-link`: archive the symlink target's contents instead of the
	// link itself.
	tar.follow_symlinks(follow_link);

	if src.is_dir() {
		// `name_override` renames the copied tree (rename-on-copy); otherwise it
		// keeps the source's own basename and lands inside the destination dir.
		let default = src.file_name().unwrap_or(std::ffi::OsStr::new("."));
		let name: &std::ffi::OsStr = name_override.map(std::ffi::OsStr::new).unwrap_or(default);
		tar.append_dir_all(name, src)
			.map_err(|e| ComposeError::Copy(format!("cp: {e}")))?;
	} else {
		let default = src.file_name().unwrap_or(std::ffi::OsStr::new("file"));
		let name: &std::ffi::OsStr = name_override.map(std::ffi::OsStr::new).unwrap_or(default);
		tar.append_path_with_name(src, name)
			.map_err(|e| ComposeError::Copy(format!("cp: {e}")))?;
	}

	let gz = tar
		.into_inner()
		.map_err(|e| ComposeError::Copy(format!("cp: {e}")))?;
	gz.finish()
		.map_err(|e| ComposeError::Copy(format!("cp: {e}")))
}

/// Route a container archive to the host destination.
///
/// docker/podman `cp` semantics for a container→host extraction:
/// - `dst` is an existing directory: the archive is extracted into it (a source
///   directory lands under its own name).
/// - `dst` does not exist and the source is a single regular file: its
///   **content** is written to exactly `dst`; the daemon-supplied entry name is
///   ignored, so a hostile image cannot choose the on-host filename (e.g. drop a
///   `.bashrc`/`authorized_keys` into the destination directory).
/// - `dst` does not exist and the source is a directory: `dst` is created and the
///   source's *contents* are copied into it (matching `docker cp`).
pub(super) fn extract_archive(tar_bytes: &[u8], dst: &Path) -> Result<()> {
	// `symlink_metadata` rather than `is_dir` so a destination that is a
	// symlink is recognised as a symlink, not the directory it points at.
	// Following the symlink here would extract the archive into the wrong
	// place (the target directory) and, once `flatten_single_wrapper_dir`
	// runs over the extracted contents, move the target's files out of it
	// (#1736). Refuse before anything touches the filesystem.
	if let Ok(meta) = std::fs::symlink_metadata(dst) {
		if meta.file_type().is_symlink() {
			return Err(ComposeError::Copy(format!(
				"cp: refusing symlink destination: {}",
				dst.display()
			)));
		}
		if meta.is_dir() {
			return extract_tar_guarded(tar_bytes, dst);
		}
	}
	// `dst` is not an existing directory.
	if !archive_contains_dir(tar_bytes)? {
		// A single regular file (or, defensively, a name-only archive). docker/podman
		// `cp` require the destination's parent to exist and refuse a trailing-slash
		// (directory) destination that does not exist, rather than auto-creating it.
		if has_trailing_separator(dst) {
			return Err(ComposeError::Copy(format!(
				"cp: no such directory: {}",
				dst.display()
			)));
		}
		if let Some(parent) = dst.parent() {
			if !parent.as_os_str().is_empty() && !parent.exists() {
				return Err(ComposeError::Copy(format!(
					"cp: no such directory: {}",
					parent.display()
				)));
			}
		}
		// The single entry's content is written to exactly `dst`;
		// `write_single_entry_to` still rejects a multi-entry archive here.
		return write_single_entry_to(tar_bytes, dst);
	}
	// Directory source. A destination that already exists as a non-directory is a
	// clear error (docker `cp` cannot copy a directory onto a file), rather than
	// letting `create_dir_all` surface a misleading "File exists".
	if dst.exists() {
		return Err(ComposeError::Copy(format!(
			"cp: cannot copy a directory onto the existing file {}",
			dst.display()
		)));
	}
	if let Some(parent) = dst.parent() {
		if !parent.as_os_str().is_empty() && !parent.exists() {
			return Err(ComposeError::Copy(format!(
				"cp: no such directory: {}",
				parent.display()
			)));
		}
	}
	// Directory source into a non-existent destination: create it and copy the
	// source's contents in. The libpod archive is tarred under the source's
	// basename, so extract through the zip-slip guard, then collapse that single
	// wrapper level to leave the contents directly under `dst`.
	std::fs::create_dir_all(dst).map_err(ComposeError::Io)?;
	extract_tar_guarded(tar_bytes, dst)?;
	flatten_single_wrapper_dir(dst)
}

/// True when the destination path was written with a trailing path separator
/// (e.g. `./newdir/`), which `docker cp` treats as an explicit *directory*
/// destination; it must already exist.
fn has_trailing_separator(p: &Path) -> bool {
	p.as_os_str()
		.to_string_lossy()
		.chars()
		.last()
		.is_some_and(std::path::is_separator)
}

/// True if any entry in `tar_bytes` is a directory, the signal that the source
/// of a container→host copy was a directory (libpod tars it under its basename),
/// as opposed to a single file.
fn archive_contains_dir(tar_bytes: &[u8]) -> Result<bool> {
	let cursor = std::io::Cursor::new(tar_bytes);
	let mut archive = tar::Archive::new(cursor);
	for entry in archive.entries().map_err(ComposeError::Io)? {
		let entry = entry.map_err(ComposeError::Io)?;
		if entry.header().entry_type() == tar::EntryType::Directory {
			return Ok(true);
		}
	}
	Ok(false)
}

/// Lift the contents of a single wrapper directory up into `dst`.
///
/// The libpod archive for `cp container:/srcdir` is tarred under the source's
/// basename (`srcdir/...`). When that lands in a freshly-created `dst`, docker
/// puts the source's *contents* directly in `dst`, so collapse the lone wrapper
/// level. A no-op unless `dst` holds exactly one entry and it is a directory.
///
/// The directory check goes through `symlink_metadata` so a wrapper that is
/// a symlink is recognised as a symlink, not the directory it points at. A
/// compromised container can plant such an entry: following the symlink would
/// read the target's contents and rename them into `dst`, emptying an
/// out-of-archive directory (#1736). The wrapper is left in place.
fn flatten_single_wrapper_dir(dst: &Path) -> Result<()> {
	let mut children: Vec<std::path::PathBuf> = std::fs::read_dir(dst)
		.map_err(ComposeError::Io)?
		.filter_map(|e| e.ok().map(|e| e.path()))
		.collect();
	if children.len() != 1 {
		return Ok(());
	}
	let wrapper = &children[0];
	if !std::fs::symlink_metadata(wrapper)
		.map(|m| m.is_dir())
		.unwrap_or(false)
	{
		return Ok(());
	}
	let wrapper = children.remove(0);
	for entry in std::fs::read_dir(&wrapper).map_err(ComposeError::Io)? {
		let from = entry.map_err(ComposeError::Io)?.path();
		let name = from
			.file_name()
			.ok_or_else(|| ComposeError::Build("cp: archive entry has no name".into()))?;
		std::fs::rename(&from, dst.join(name)).map_err(ComposeError::Io)?;
	}
	std::fs::remove_dir(&wrapper).map_err(ComposeError::Io)?;
	Ok(())
}

/// Extract a (plain, uncompressed) tar archive into `dst_dir`, refusing any
/// Rebuild the on-disk path `unpack_in` chose for an entry.
///
/// `unpack_in` keeps only [`Component::Normal`] parts (a leading `/` or a `..`
/// is dropped rather than honoured) so an entry named `/etc/passwd` lands at
/// `dst_dir/etc/passwd`. Reconstructing that path as `dst_dir.join(rel)` is
/// wrong in exactly that case: [`Path::join`] with an absolute path **discards
/// the base**, so the follow-up chmod would leave the destination and land on
/// the real host file: a hostile image could hand back an entry named
/// `/home/user/.ssh/id_ed25519` with mode 0o644 and have the copy loosen it.
/// Mirroring `unpack_in`'s own rule keeps the two in agreement.
///
/// Returns `None` when no normal component survives (`/`, `..`, `.`), which is
/// nothing to chmod.
fn unpacked_path(dst_dir: &Path, rel: &Path) -> Option<std::path::PathBuf> {
	let mut out = dst_dir.to_path_buf();
	let mut pushed = false;
	for component in rel.components() {
		if let std::path::Component::Normal(part) = component {
			out.push(part);
			pushed = true;
		}
	}
	pushed.then_some(out)
}

/// entry whose path would escape it (zip-slip) and stripping group/other-write
/// and setuid/setgid/sticky bits the (untrusted) container set on each entry.
///
/// Extract entry-by-entry rather than `archive.unpack`: a malicious or
/// compromised container can craft tar entries whose paths contain `..` or are
/// absolute, escaping `dst_dir` and overwriting host files. `unpack_in` refuses
/// such entries, returning `Ok(false)`; we turn that into a hard error so the
/// copy fails loudly instead of silently skipping data. Pure and synchronous so
/// the guard can be unit-tested without a container.
///
/// `pub fn` inside the crate-private `archive` module: the path through
/// `engine` (which is `pub(crate)`) keeps the function off the public API, and
/// the fuzz harness behind the `test-helpers` feature reaches it through
/// `crate::fuzz_api`.
pub fn extract_tar_guarded<R: std::io::Read>(src: R, dst_dir: &Path) -> Result<()> {
	let mut archive = tar::Archive::new(src);
	for entry in archive.entries().map_err(ComposeError::Io)? {
		let mut entry = entry.map_err(ComposeError::Io)?;
		let rel = entry.path().map(|p| p.into_owned()).ok();
		let mode = entry.header().mode().ok();
		if !entry.unpack_in(dst_dir).map_err(ComposeError::Io)? {
			let p = entry
				.path()
				.map(|p| p.display().to_string())
				.unwrap_or_else(|_| "<unprintable>".into());
			return Err(ComposeError::Build(format!(
				"cp: refusing archive entry that escapes destination: {p}"
			)));
		}
		match (rel, mode) {
			(Some(rel), Some(mode)) => match unpacked_path(dst_dir, &rel) {
				Some(path) => sanitize_extracted_mode(&path, mode),
				None => {
					tracing::warn!("cp: entry with no usable path component; mode not sanitized")
				}
			},
			_ => tracing::warn!("cp: unreadable entry path or mode; mode not sanitized"),
		}
	}
	Ok(())
}

/// Write the single regular-file entry of `tar_bytes` to exactly `dst`,
/// honouring the user's destination filename rather than the daemon's. Errors
/// if the archive is empty, holds more than one entry, or the entry is not a
/// regular file (those cases only make sense against a directory destination).
fn write_single_entry_to(tar_bytes: &[u8], dst: &Path) -> Result<()> {
	use std::io::Read;

	// The caller (`extract_archive`) has already validated that `dst`'s parent
	// directory exists; matching docker/podman `cp`, a missing parent is an error
	// rather than being auto-created here.

	let cursor = std::io::Cursor::new(tar_bytes);
	let mut archive = tar::Archive::new(cursor);
	let mut written = false;
	for entry in archive.entries().map_err(ComposeError::Io)? {
		let mut entry = entry.map_err(ComposeError::Io)?;
		if entry.header().entry_type() != tar::EntryType::Regular {
			return Err(ComposeError::Unsupported(format!(
				"cp: destination {} is not a directory but the source is not a single file",
				dst.display()
			)));
		}
		if written {
			return Err(ComposeError::Unsupported(format!(
				"cp: destination {} is not a directory but the source has multiple entries",
				dst.display()
			)));
		}
		let mode = entry.header().mode().ok();
		let mut buf = Vec::new();
		entry.read_to_end(&mut buf).map_err(ComposeError::Io)?;
		std::fs::write(dst, &buf).map_err(ComposeError::Io)?;
		if let Some(mode) = mode {
			sanitize_extracted_mode(dst, mode);
		}
		written = true;
	}
	if !written {
		return Err(ComposeError::Build(
			"cp: container archive was empty".into(),
		));
	}
	Ok(())
}

/// Strip group/other-write and setuid/setgid/sticky bits from a file extracted
/// from an untrusted container, keeping the owner and read/execute bits. No-op
/// on non-files (e.g. symlinks) and on non-unix platforms.
#[cfg(unix)]
fn sanitize_extracted_mode(path: &Path, mode: u32) {
	use std::os::unix::fs::PermissionsExt;
	let Ok(meta) = std::fs::symlink_metadata(path) else {
		return;
	};
	if !meta.is_file() {
		return;
	}
	let masked = mode & 0o7777 & !0o022 & !0o7000;
	if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(masked)) {
		tracing::warn!("cp: could not set permissions on {}: {e}", path.display());
	}
}

#[cfg(not(unix))]
fn sanitize_extracted_mode(_path: &Path, _mode: u32) {}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "archive_symlink_tests.rs"]
mod symlink_tests;
