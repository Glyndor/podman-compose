//! `cp` command: copy files between a service container and the host.

use std::path::Path;

use bytes::Bytes;
use http_body_util::{BodyExt, Limited};

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::client::PathStat;
use crate::libpod::urlencoded;
use crate::libpod::API_PREFIX;

use super::Engine;

/// Crate-private so the fuzz harness behind the `test-helpers` feature can
/// reach `extract_tar_guarded` without widening the published API surface.
pub(crate) mod archive;
mod stream;

use archive::{extract_archive, pack_path};

/// Upper bound on a container→host `cp` archive buffered in memory. Without it a
/// hostile or huge container path would OOM the CLI. Generous (covers ordinary
/// file/dir copies); larger transfers should use `podman cp` directly.
const MAX_CP_ARCHIVE_BYTES: usize = 1024 * 1024 * 1024;

/// Options for [`Engine::cp_with_options`], mirroring `docker compose cp` flags.
///
/// `#[non_exhaustive]` since 4.0.0, so a new field can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`CpOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
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

impl CpOptions {
	/// Every `docker compose cp` flag, in CLI order. A constructor rather than
	/// a struct literal because the type is `#[non_exhaustive]`, so the next
	/// flag to land is not a breaking change for anyone building one.
	pub fn new(index: Option<u32>, follow_link: bool, archive: bool) -> Self {
		Self {
			index,
			follow_link,
			archive,
		}
	}

	/// 1-based replica index for a scaled service, `--index` (default: first).
	/// Builder-style.
	#[must_use]
	pub fn with_index(mut self, index: Option<u32>) -> Self {
		self.index = index;
		self
	}

	/// Follow symlinks in the host source before packing, `-L/--follow-link`.
	/// Builder-style.
	#[must_use]
	pub fn with_follow_link(mut self, follow_link: bool) -> Self {
		self.follow_link = follow_link;
		self
	}

	/// Archive mode, `-a/--archive`. Accepted for command-line compatibility:
	/// under rootless Podman the original uid/gid cannot be restored, and
	/// container→host extraction always applies podup's security-hardened mode
	/// sanitization, so this flag has no effect on the copied bytes.
	/// Builder-style.
	#[must_use]
	pub fn with_archive(mut self, archive: bool) -> Self {
		self.archive = archive;
		self
	}
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
	/// `-a/--archive` (accepted for compatibility; see [`CpOptions::archive`]).
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
		let dst = dst.to_path_buf();

		// Two destination shapes, and only one of them can be streamed.
		//
		// An existing directory goes straight to `extract_tar_guarded`, which
		// walks the archive once, so the body can be piped into it and nothing
		// accumulates. That is also the shape that moves bulk data (`cp
		// svc:/var/lib/data ./backup/`), which is why it is the one worth
		// streaming.
		//
		// Any other destination goes through `extract_archive`, which reads the
		// archive twice: `archive_contains_dir` decides whether the destination
		// names a file or a directory to create, and only then does it extract.
		// A stream cannot be rewound, so that path still buffers. Making it
		// single-pass means changing what `cp` does with an ambiguous
		// destination, which is a behaviour decision rather than a memory one.
		if dst.is_dir() {
			return stream::extract_streamed(resp, dst, MAX_CP_ARCHIVE_BYTES as u64).await;
		}

		// Cap the buffered archive so a huge/hostile container path cannot OOM
		// the CLI (the streaming `get_stream` path bypasses the client's own
		// cap). Kept as `Bytes` rather than `.to_vec()`: the extractor takes
		// `&[u8]`, which `Bytes` derefs to, and a `.to_vec()` here would hold a
		// second copy alive beside the first.
		let tar_bytes: Bytes = Limited::new(resp.into_body(), MAX_CP_ARCHIVE_BYTES)
			.collect()
			.await
			.map_err(|_| {
				ComposeError::Unsupported(format!(
					"cp: container archive exceeds {MAX_CP_ARCHIVE_BYTES} bytes; \
					 copy fewer files or use `podman cp` for very large transfers"
				))
			})?
			.to_bytes();

		tokio::task::spawn_blocking(move || extract_archive(&tar_bytes, &dst))
			.await
			.map_err(|e| ComposeError::Build(e.to_string()))??;

		Ok(())
	}

	/// Push a host file or directory into a service container.
	///
	/// # Concurrency contract: read before touching the two HEAD + PUT sequence
	///
	/// `cp_to_container` issues two `HEAD /archive` requests with the PUT
	/// between them, so a concurrent mutation in the window could land a
	/// successful-but-wrong-state PUT. The two callers in the codebase are
	/// the CLI `cp` subcommand and the `watch` sync path, both of which are
	/// called only while the per-project lock ([`crate::engine::lock`]) is
	/// held by the mutating stage: `lock_project` serialises a single
	/// `podup` process against any other `podup` process working on the same
	/// project, closing the **cross-invocation** case.
	///
	/// The **within-invocation** case, a foreign actor (a manual
	/// `podman exec`, another compose stack on the same machine, the user
	/// running `podman cp` in another shell) mutating the destination
	/// between the two HEADs, is closed by libpod itself: the archive PUT
	/// extracts into a directory that we have just confirmed exists and is
	/// a directory, so a foreign `rm -rf` racing in is rejected by the
	/// second PUT, not silently succeeded. The `extract_stat_path` HEAD
	/// below is what makes that property hold; do not skip it.
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
		let uploaded_size = uploaded_entry_size(src);
		self.put_archive_verified(
			&container_name,
			&extract_dir,
			&entry,
			tar_bytes,
			uploaded_size,
		)
		.await
	}

	/// PUT a gzipped tar to a container's archive endpoint at `dir`, extracting
	/// it there, and confirm it landed, the upload path shared by `cp` and
	/// `watch` sync.
	///
	/// #1097: on Podman 6 the archive endpoint applies the tar and then closes
	/// the connection *without* an HTTP response, which hyper reports as
	/// `IncompleteMessage` even though the copy landed (the content does appear,
	/// measured on 6.0.1; every raw request to the same endpoint gets a clean
	/// 200, so the trigger is client-side and could not be stripped out). To tell
	/// that apply-then-close apart from a *genuine* upload failure (a dropped
	/// socket, a truncated body), read `dir/entry` after the PUT and treat the
	/// copy as landed only if it now **matches what was uploaded**, which is what
	/// `uploaded_size` carries.
	///
	/// This used to compare the entry's mtime before and after and require it to
	/// move. That signal cannot express the question: Podman 6 reports the mtime
	/// to whole seconds, so two copies inside one second look identical
	/// (#1270: three failures in six back-to-back copies, measured), and
	/// re-copying an *unchanged* file is undetectable at any resolution because
	/// the extracted file takes the source's own mtime.
	///
	/// Fails, rather than guessing, when the entry has no name (`cp . svc:/`),
	/// when the source size is unknown, or when the post-PUT stat cannot be read.
	///
	/// Known limit: a *directory* entry has no size to compare, so re-syncing a
	/// tree is reported as unverifiable (fail-closed, never a false success).
	/// Inert on Podman 5, which returns a normal response.
	pub(super) async fn put_archive_verified(
		&self,
		container: &str,
		dir: &str,
		entry: &str,
		tar_bytes: Vec<u8>,
		uploaded_size: Option<u64>,
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
		// What the destination entry must look like once the archive is applied.
		//
		// This used to read the entry's mtime *before* the PUT and check that it
		// moved afterwards. That cannot work: Podman 6 reports the mtime to
		// whole seconds, so two copies inside one second are indistinguishable
		// (measured at three failures in six back-to-back copies, #1270), and
		// copying an unchanged file twice is undetectable at any resolution,
		// because the extracted file takes the source's own mtime.
		//
		// The question the confirmation should ask is not "did the entry
		// change" but "does the entry now match what was uploaded". `None`
		// means the answer is unknowable (no verifiable entry, or the source
		// could not be stat'd) and forces a later IncompleteMessage to fail
		// rather than guess.
		let expected = verify_path
			.as_ref()
			.and(uploaded_size)
			.map(|size| ExpectedEntry { size });

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
		let landed = match (&verify_path, &expected) {
			(Some(p), Some(want)) => match self.client.head_path_stat(p).await {
				Ok(post) => copy_landed(want, post.as_ref()),
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
			"the upload to {dir} could not be confirmed: the container runtime closed the \
			 connection without a response and the destination did not change. The copy may \
			 or may not have landed; check {dir} in the container."
		)))
	}
}

/// The size the destination entry must end up with, or `None` when there is
/// nothing to compare.
///
/// Only a regular file has a size the archive preserves. A directory upload
/// stays unverifiable and therefore fail-closed, which is what it was before:
/// a directory entry's own size says nothing about whether its children
/// arrived. A source that cannot be stat'd is `None` for the same reason:
/// unknown must not become a guess.
///
/// Extracted so it is reachable from a test. Inside the async upload it was
/// covered only by running against a real container, and a mutation replacing
/// the real length with a constant survived the whole unit suite.
pub(super) fn uploaded_entry_size(src: &std::path::Path) -> Option<u64> {
	std::fs::metadata(src)
		.ok()
		.filter(std::fs::Metadata::is_file)
		.map(|m| m.len())
}

/// What the destination entry must look like for the upload to have landed.
///
/// Only the size for now. The mtime is deliberately not part of it: the archive
/// sets it from the source, but Podman reports it to whole seconds while the
/// source's own mtime carries sub-second precision, so comparing the two would
/// re-introduce a resolution mismatch, this time as a false *negative* on a
/// copy that did land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExpectedEntry {
	size: u64,
}

/// Whether a `cp`/sync whose archive PUT ended in an `IncompleteMessage`
/// actually landed, by comparing the destination entry against what was
/// uploaded.
///
/// The entry must exist and its size must equal the source's. **Not "did it
/// change"**, which is what this asked before: Podman 6's mtime has one-second
/// resolution, so a second copy inside the same second reported an unchanged
/// mtime and a copy that had landed was called a failure (#1270, measured at
/// three failures in six). Copying an unchanged file twice was undetectable at
/// any resolution, since the extracted file takes the source's own mtime.
///
/// The residual false positive is a failed upload onto an entry that already
/// happened to be the same size. It is benign in a way the old false negative
/// was not: the destination already holds bytes of the length the caller
/// intended, and the caller is told the copy succeeded rather than being told a
/// successful copy failed.
///
/// Pure so the decision is unit-tested without a container.
fn copy_landed(expected: &ExpectedEntry, post: Option<&PathStat>) -> bool {
	post.is_some_and(|entry| entry.size == expected.size)
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
	// `SERVICE:PATH`: colon must not be the first character and path cannot be empty.
	let (svc, path) = s.split_once(':')?;
	if svc.is_empty() || path.is_empty() {
		return None;
	}
	// On Windows, an absolute path like `C:\path` has a single-char drive prefix,
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
#[path = "copy_tests.rs"]
mod tests;
