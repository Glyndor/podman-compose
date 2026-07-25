//! `cp` command: copy files between a service container and the host.

use std::path::Path;

use bytes::Bytes;
use http_body_util::{BodyExt, Limited};

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::urlencoded;
use crate::libpod::API_PREFIX;

use super::Engine;

mod archive;

use archive::{extract_archive, pack_path};

/// Upper bound on a container→host `cp` archive buffered in memory. Without it a
/// hostile or huge container path would OOM the CLI. Generous (covers ordinary
/// file/dir copies); larger transfers should use `podman cp` directly.
const MAX_CP_ARCHIVE_BYTES: usize = 1024 * 1024 * 1024;

/// Options for [`Engine::cp_with_options`], mirroring `docker compose cp` flags.
#[derive(Default)]
pub struct CpOptions {
	/// 1-based replica index for a scaled service, `--index` (default: first).
	pub index: Option<u32>,
	/// Follow symlinks in the host source before packing, `-L/--follow-link`.
	pub follow_link: bool,
	/// Archive mode, `-a/--archive`. Accepted for command-line compatibility:
	/// under rootless Podman the original uid/gid cannot be restored, and
	/// container→host extraction always applies podup's security-hardened mode
	/// sanitization, so this flag has no effect on the copied bytes.
	pub archive: bool,
}

impl Engine {
	/// Copy between a service container and the local filesystem.
	///
	/// Either `src` or `dst` (but not both) must have the form `SERVICE:PATH`.
	/// The other side is a local path. `SERVICE:-` / `-:SERVICE` for stdin/stdout
	/// is not supported.
	pub async fn cp(&self, file: &ComposeFile, src: &str, dst: &str) -> Result<()> {
		self.cp_with_options(file, src, dst, CpOptions::default())
			.await
	}

	/// Copy with `docker compose cp` options: `--index` (target a specific
	/// replica), `-L/--follow-link` (follow host symlinks when uploading) and
	/// `-a/--archive` (accepted for compatibility — see [`CpOptions::archive`]).
	pub async fn cp_with_options(
		&self,
		file: &ComposeFile,
		src: &str,
		dst: &str,
		opts: CpOptions,
	) -> Result<()> {
		// Reject the explicitly-unsupported endpoint forms (`-` for stdin/stdout,
		// and a `SERVICE:` with an empty container path) with a clear message
		// before they silently fall through to a local file literally named `-`
		// or `SERVICE:`.
		check_endpoint(src)?;
		check_endpoint(dst)?;
		match (parse_endpoint(src), parse_endpoint(dst)) {
			(Some((service, container_path)), None) => {
				self.cp_from_container(file, service, container_path, Path::new(dst), &opts)
					.await
			}
			(None, Some((service, container_path))) => {
				self.cp_to_container(file, service, Path::new(src), container_path, &opts)
					.await
			}
			(Some(_), Some(_)) => Err(ComposeError::Unsupported(
				"cp: both src and dst cannot be SERVICE:PATH".into(),
			)),
			(None, None) => Err(ComposeError::Unsupported(
				"cp: one of src or dst must be SERVICE:PATH".into(),
			)),
		}
	}

	async fn cp_from_container(
		&self,
		file: &ComposeFile,
		service_name: &str,
		container_path: &str,
		dst: &Path,
		opts: &CpOptions,
	) -> Result<()> {
		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into()))?;
		let container_name = self
			.live_replica_name_at(service_name, service, opts.index)
			.await?;

		let path = format!(
			"{API_PREFIX}/containers/{}/archive?path={}",
			urlencoded(&container_name),
			urlencoded(container_path),
		);
		let resp = self
			.client
			.get_stream(&path)
			.await
			.map_err(ComposeError::Podman)?;
		// Cap the buffered archive so a huge/hostile container path cannot OOM the
		// CLI (the streaming `get_stream` path bypasses the client's own cap).
		let tar_bytes = Limited::new(resp.into_body(), MAX_CP_ARCHIVE_BYTES)
			.collect()
			.await
			.map_err(|_| {
				ComposeError::Unsupported(format!(
					"cp: container archive exceeds {MAX_CP_ARCHIVE_BYTES} bytes; \
					 copy fewer files or use `podman cp` for very large transfers"
				))
			})?
			.to_bytes()
			.to_vec();

		let dst = dst.to_path_buf();
		tokio::task::spawn_blocking(move || extract_archive(&tar_bytes, &dst))
			.await
			.map_err(|e| ComposeError::Build(e.to_string()))??;

		Ok(())
	}

	async fn cp_to_container(
		&self,
		file: &ComposeFile,
		service_name: &str,
		src: &Path,
		container_path: &str,
		opts: &CpOptions,
	) -> Result<()> {
		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into()))?;
		let container_name = self
			.live_replica_name_at(service_name, service, opts.index)
			.await?;

		// Match `docker cp` destination semantics. The libpod archive PUT extracts
		// the tar *at* a directory, so:
		//  - dest is an existing directory (or ends in `/`)  → copy the source in
		//    under its own name (PUT to the dest dir);
		//  - dest is anything else (a new name, or a file)   → rename the source to
		//    the dest's basename and PUT to the dest's parent.
		// Without this, `cp file svc:/path/newname` created `newname/` as a
		// directory holding the source instead of a file named `newname`.
		let stat_path = format!(
			"{API_PREFIX}/containers/{}/archive?path={}",
			urlencoded(&container_name),
			urlencoded(container_path),
		);
		let dest_is_dir = self.client.head_path_is_dir(&stat_path).await? == Some(true);

		let (extract_dir, rename) = if dest_is_dir || container_path.ends_with('/') {
			(container_path.trim_end_matches('/').to_string(), None)
		} else {
			let trimmed = container_path.trim_end_matches('/');
			let (parent, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
			let parent = if parent.is_empty() { "/" } else { parent };
			(parent.to_string(), Some(name.to_string()))
		};

		// Validate the extraction directory exists and is itself a directory before
		// PUTting the archive. Without this, libpod silently auto-creates a missing
		// parent chain (diverging from docker/podman `cp`, which error with "no such
		// directory"); and when a path component is a regular file the archive PUT
		// never gets a response, blocking the full READ_TIMEOUT window instead of
		// failing fast.
		let extract_stat_path = format!(
			"{API_PREFIX}/containers/{}/archive?path={}",
			urlencoded(&container_name),
			urlencoded(&extract_dir),
		);
		match self.client.head_path_is_dir(&extract_stat_path).await? {
			Some(true) => {}
			Some(false) => {
				return Err(ComposeError::Copy(format!(
					"cp: not a directory: {extract_dir}"
				)));
			}
			None => {
				return Err(ComposeError::Copy(format!(
					"cp: no such directory: {extract_dir}"
				)));
			}
		}

		let src_buf = src.to_path_buf();
		let follow = opts.follow_link;
		let rename_for_pack = rename.clone();
		let tar_bytes = tokio::task::spawn_blocking(move || {
			pack_path(&src_buf, follow, rename_for_pack.as_deref())
		})
		.await
		.map_err(|e| ComposeError::Build(e.to_string()))??;

		let entry = rename.clone().unwrap_or_else(|| {
			src.file_name()
				.map(|n| n.to_string_lossy().into_owned())
				.unwrap_or_default()
		});
		self.put_archive_verified(&container_name, &extract_dir, &entry, tar_bytes)
			.await
	}

	/// PUT a gzipped tar to a container's archive endpoint at `dir`, extracting
	/// it there, and confirm it landed — the upload path shared by `cp` and
	/// `watch` sync.
	///
	/// #1097: on Podman 6 the archive endpoint applies the tar and then closes
	/// the connection *without* an HTTP response, which hyper reports as
	/// `IncompleteMessage` even though the copy landed (the content does appear —
	/// measured on 6.0.1; every raw request to the same endpoint gets a clean
	/// 200, so the trigger is client-side and could not be stripped out). To tell
	/// that apply-then-close apart from a *genuine* upload failure (a dropped
	/// socket, a truncated body), capture `dir/entry`'s mtime before the PUT and,
	/// on an `IncompleteMessage`, treat the copy as landed only if that mtime
	/// moved — the extracted file takes the source's mtime, so an unchanged one
	/// means a failed upload left the old entry in place. Fails, rather than
	/// guessing, when the entry has no name (`cp . svc:/`), when the pre- or
	/// post-PUT stat cannot be read, or when the mtime did not move.
	///
	/// Known limit: the mtime of a *directory* entry moves only when its own
	/// children are added or removed, so re-syncing a tree whose only change is
	/// deeper than the top level is reported as unverifiable (fail-closed, never a
	/// false success). Inert on Podman 5, which returns a normal response.
	pub(super) async fn put_archive_verified(
		&self,
		container: &str,
		dir: &str,
		entry: &str,
		tar_bytes: Vec<u8>,
	) -> Result<()> {
		let path = format!(
			"{API_PREFIX}/containers/{}/archive?path={}",
			urlencoded(container),
			urlencoded(dir),
		);
		let verify_path = (!entry.is_empty()).then(|| {
			format!(
				"{API_PREFIX}/containers/{}/archive?path={}",
				urlencoded(container),
				urlencoded(&join_archive_path(dir, entry)),
			)
		});
		// The pre-PUT mtime, only when it can be read cleanly. `None` means
		// "unknown" (no verifiable entry, or the stat failed) and forces a later
		// IncompleteMessage to fail rather than guess.
		let pre_mtime = match &verify_path {
			Some(p) => self.client.head_path_mtime(p).await.ok(),
			None => None,
		};

		// `application/gzip` is the honest label for the gzipped tar; Podman
		// sniffs the magic bytes and forgives either.
		let Err(e) = self
			.client
			.put_bytes_ok(&path, Bytes::from(tar_bytes), "application/gzip")
			.await
		else {
			return Ok(());
		};
		// Only the Podman-6 apply-then-close is recoverable; any other error is a
		// genuine failure and propagates unchanged.
		if !e.is_incomplete_message() {
			return Err(ComposeError::Podman(e));
		}
		let landed = match (&verify_path, pre_mtime) {
			(Some(p), Some(pre)) => match self.client.head_path_mtime(p).await {
				Ok(post) => copy_landed(pre.as_deref(), post.as_deref()),
				Err(stat_err) => {
					tracing::debug!(
						"cp: could not re-verify {p} after an incomplete PUT: {stat_err}"
					);
					false
				}
			},
			_ => false,
		};
		if landed {
			return Ok(());
		}
		// The upload finished but its result could not be confirmed. Say so, with
		// an actionable hint, instead of surfacing the raw transport error.
		Err(ComposeError::Copy(format!(
			"the upload to {dir} could not be confirmed — the container runtime closed the \
			 connection without a response and the destination did not change. The copy may \
			 or may not have landed; check {dir} in the container."
		)))
	}
}

/// Whether a `cp`/sync whose archive PUT ended in an `IncompleteMessage`
/// actually landed, from the destination entry's mtime before and after the
/// PUT. The entry must exist now (`post` is `Some`) and its mtime must differ
/// from before — the extracted file takes the source's mtime, so an unchanged
/// mtime means a failed upload left the old entry in place. A vanished entry, or
/// one whose mtime did not move, is "did not land". Pure so the decision is
/// unit-tested without a container.
fn copy_landed(pre: Option<&str>, post: Option<&str>) -> bool {
	post.is_some() && post != pre
}

/// Join a container directory and an entry name into one path, without doubling
/// the separator when the directory already ends in `/` (so root `/` yields
/// `/name`, not `//name`). Pure so the join is unit-tested without a container.
fn join_archive_path(dir: &str, entry: &str) -> String {
	if dir.ends_with('/') {
		format!("{dir}{entry}")
	} else {
		format!("{dir}/{entry}")
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Reject the `cp` endpoint forms podup explicitly does not support, with a
/// clear diagnostic rather than letting them fall through to a local path that
/// happens to be named `-` or `SERVICE:`.
///
/// - `-` (stdin/stdout streaming) is not implemented.
/// - `SERVICE:` (a colon with an empty container path) is a malformed reference.
///
/// A plain local path (no colon, or a colon that is part of an ordinary host
/// path / Windows drive) is left to [`parse_endpoint`].
fn check_endpoint(s: &str) -> Result<()> {
	if s == "-" {
		return Err(ComposeError::Unsupported(
			"cp: stdin/stdout ('-') is not supported".into(),
		));
	}
	if let Some((svc, path)) = s.split_once(':') {
		// A Windows drive letter (`C:\...`) is a local path, not a service ref.
		#[cfg(windows)]
		if svc.len() == 1 && svc.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
			return Ok(());
		}
		if !svc.is_empty() && path.is_empty() {
			return Err(ComposeError::Copy(format!(
				"cp: empty container path in '{s}' (expected SERVICE:PATH)"
			)));
		}
	}
	Ok(())
}

fn parse_endpoint(s: &str) -> Option<(&str, &str)> {
	if s == "-" {
		return None;
	}
	// `SERVICE:PATH` — colon must not be the first character and path cannot be empty.
	let (svc, path) = s.split_once(':')?;
	if svc.is_empty() || path.is_empty() {
		return None;
	}
	// On Windows, an absolute path like `C:\path` has a single-char drive prefix —
	// treat those as local paths, not service endpoints. This must NOT apply on
	// Unix, where a one-character service name (`c:/path`) is perfectly valid and
	// would otherwise be rejected as a bogus "drive".
	#[cfg(windows)]
	if svc.len() == 1 && svc.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
		return None;
	}
	Some((svc, path))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
	use super::{copy_landed, join_archive_path, parse_endpoint};

	#[test]
	fn join_archive_path_does_not_double_the_separator() {
		// The #1097 re-verify stats `<dir>/<entry>`; a dir already ending in `/`
		// (notably root) must not produce `//entry`, which libpod reads as a
		// different path and 404s, turning a landed copy into a false failure.
		assert_eq!(join_archive_path("/tmp", "f.txt"), "/tmp/f.txt");
		assert_eq!(join_archive_path("/tmp/", "f.txt"), "/tmp/f.txt");
		assert_eq!(join_archive_path("/", "f.txt"), "/f.txt");
	}

	#[test]
	fn copy_landed_requires_the_entry_to_exist_and_its_mtime_to_move() {
		// A brand-new destination: absent before, present after -> landed.
		assert!(copy_landed(None, Some("2026-01-01T00:00:00Z")));
		// An overwrite that applied: the entry's mtime changed to the source's.
		assert!(copy_landed(
			Some("2026-01-01T00:00:00Z"),
			Some("2026-06-01T00:00:00Z")
		));
		// An overwrite whose PUT actually failed: the old entry is still there,
		// unchanged. This is the silent-success the mtime check exists to prevent.
		assert!(!copy_landed(
			Some("2026-01-01T00:00:00Z"),
			Some("2026-01-01T00:00:00Z")
		));
		// The entry vanished (or never appeared): not landed.
		assert!(!copy_landed(Some("2026-01-01T00:00:00Z"), None));
		assert!(!copy_landed(None, None));
	}

	#[test]
	fn parse_service_colon_path() {
		assert_eq!(parse_endpoint("web:/app/data"), Some(("web", "/app/data")));
	}

	#[test]
	fn parse_local_path_no_colon() {
		assert_eq!(parse_endpoint("/tmp/file.txt"), None);
	}

	#[test]
	fn parse_dash_is_local() {
		assert_eq!(parse_endpoint("-"), None);
	}

	#[cfg(windows)]
	#[test]
	fn parse_windows_drive_letter_is_local() {
		assert_eq!(parse_endpoint("C:\\Users\\foo"), None);
	}

	#[cfg(not(windows))]
	#[test]
	fn single_char_service_parses_on_unix() {
		// On Unix a one-character service name is valid; only Windows treats a
		// single-char prefix as a drive letter.
		assert_eq!(parse_endpoint("c:/tmp/file"), Some(("c", "/tmp/file")));
		assert_eq!(parse_endpoint("w:data"), Some(("w", "data")));
	}

	#[test]
	fn parse_empty_service_or_path() {
		assert_eq!(parse_endpoint(":path"), None);
		assert_eq!(parse_endpoint("svc:"), None);
	}

	#[cfg(windows)]
	#[test]
	fn parse_windows_drive_letter_forward_slash() {
		assert_eq!(parse_endpoint("C:/Users/foo"), None);
	}

	#[test]
	fn parse_service_with_relative_path() {
		assert_eq!(
			parse_endpoint("web:data/file.txt"),
			Some(("web", "data/file.txt"))
		);
	}

	#[test]
	fn parse_service_name_with_dots() {
		assert_eq!(
			parse_endpoint("my.service:/app/config"),
			Some(("my.service", "/app/config"))
		);
	}

	#[test]
	fn check_endpoint_rejects_dash() {
		let err = super::check_endpoint("-").unwrap_err();
		assert!(format!("{err}").contains("stdin/stdout"), "got: {err}");
	}

	#[test]
	fn check_endpoint_rejects_empty_container_path() {
		let err = super::check_endpoint("web:").unwrap_err();
		assert!(
			format!("{err}").contains("empty container path"),
			"got: {err}"
		);
	}

	#[test]
	fn check_endpoint_allows_normal_forms() {
		// A plain local path, a proper SERVICE:PATH, and a relative host path are
		// all fine (validation only rejects `-` and `SERVICE:`).
		assert!(super::check_endpoint("/tmp/file").is_ok());
		assert!(super::check_endpoint("web:/app/data").is_ok());
		assert!(super::check_endpoint("./local").is_ok());
	}
}
