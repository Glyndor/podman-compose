//! `commit` and `export`: snapshot a service container to an image, or stream
//! its filesystem out as a tar archive (`docker compose commit` / `export`).

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use http_body_util::BodyExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::super::Engine;

/// Options for [`Engine::commit_with_options`], mirroring `docker compose commit`
/// (`-m/--message`, `-a/--author`, `-p/--pause`, `-c/--change`).
///
/// `#[non_exhaustive]` since 4.0.0, so a new field can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`CommitOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CommitOptions {
	/// Commit message recorded on the new image (`-m/--message`).
	pub message: Option<String>,
	/// Author recorded on the new image (`-a/--author`).
	pub author: Option<String>,
	/// Pause the container during the commit (`-p/--pause`); `None` leaves
	/// Podman's default (pause on).
	pub pause: Option<bool>,
	/// Dockerfile instructions to apply to the committed image (`-c/--change`).
	pub changes: Vec<String>,
}

impl CommitOptions {
	/// Every `docker compose commit` field, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next field to land is not a breaking change for anyone building one.
	pub fn new(
		message: Option<String>,
		author: Option<String>,
		pause: Option<bool>,
		changes: Vec<String>,
	) -> Self {
		Self {
			message,
			author,
			pause,
			changes,
		}
	}

	/// Commit message recorded on the new image (`-m/--message`).
	/// Builder-style.
	#[must_use]
	pub fn with_message(mut self, message: Option<String>) -> Self {
		self.message = message;
		self
	}

	/// Author recorded on the new image (`-a/--author`). Builder-style.
	#[must_use]
	pub fn with_author(mut self, author: Option<String>) -> Self {
		self.author = author;
		self
	}

	/// Pause the container during the commit (`-p/--pause`); `None` leaves
	/// Podman's default (pause on). Builder-style.
	#[must_use]
	pub fn with_pause(mut self, pause: Option<bool>) -> Self {
		self.pause = pause;
		self
	}

	/// Dockerfile instructions to apply to the committed image
	/// (`-c/--change`). Builder-style.
	#[must_use]
	pub fn with_changes(mut self, changes: Vec<String>) -> Self {
		self.changes = changes;
		self
	}
}

/// Split a `commit` image reference into `(repo, tag)`, defaulting the tag to
/// `latest`. Rejects an empty reference or an empty repository (`""` or `:tag`),
/// which podman would otherwise accept and turn into a dangling `<none>` image.
/// Pure so it is unit-tested.
fn split_image_ref(image: &str) -> Result<(&str, &str)> {
	let (repo, tag) = match image.rsplit_once(':') {
		// A ':' after the last '/' is a tag; otherwise it's part of a registry
		// host:port and the whole string is the repo.
		Some((r, t)) if !t.contains('/') => (r, t),
		_ => (image, "latest"),
	};
	if repo.is_empty() {
		return Err(ComposeError::Unsupported(format!(
			"invalid image reference {image:?}: a non-empty repository name is required \
			 (e.g. myimage or myimage:tag)"
		)));
	}
	if tag.is_empty() {
		return Err(ComposeError::Unsupported(format!(
			"invalid image reference {image:?}: the tag after ':' must not be empty"
		)));
	}
	Ok((repo, tag))
}

/// Build the libpod `/commit` request path. `pause` quiesces the container
/// during the snapshot (docker default) for a consistent filesystem.
fn commit_path(container: &str, repo: &str, tag: &str, pause: bool) -> String {
	format!(
		"{API_PREFIX}/commit?container={}&repo={}&tag={}&pause={pause}",
		urlencoded(container),
		urlencoded(repo),
		urlencoded(tag),
	)
}

impl Engine {
	/// Commit a service container to a new image (`docker compose commit`),
	/// pausing the container during the snapshot for a consistent filesystem
	/// (docker default). Equivalent to [`Engine::commit_with_pause`] with
	/// `pause = true`. Targets the given replica (`index`, 1-based) or the first
	/// one. `image` is `repo[:tag]`; an omitted tag defaults to `latest`.
	pub async fn commit(
		&self,
		file: &ComposeFile,
		service_name: &str,
		image: &str,
		index: Option<u32>,
	) -> Result<()> {
		self.commit_with_pause(file, service_name, image, index, true)
			.await
	}

	/// Commit a service container to a new image (`docker compose commit`).
	/// Targets the given replica (`index`, 1-based) or the first one. `image`
	/// is `repo[:tag]`; an omitted tag defaults to `latest`. `pause` quiesces the
	/// container during the snapshot for a consistent filesystem (docker default).
	pub async fn commit_with_pause(
		&self,
		file: &ComposeFile,
		service_name: &str,
		image: &str,
		index: Option<u32>,
		pause: bool,
	) -> Result<()> {
		self.commit_with_options(
			file,
			service_name,
			image,
			index,
			CommitOptions {
				pause: Some(pause),
				..Default::default()
			},
		)
		.await
	}

	/// Commit a service container with `docker compose commit`-style options
	/// (message, author, pause, change).
	pub async fn commit_with_options(
		&self,
		file: &ComposeFile,
		service_name: &str,
		image: &str,
		index: Option<u32>,
		opts: CommitOptions,
	) -> Result<()> {
		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into()))?;
		let container = self.replica_name_at(service_name, service, index)?;

		let (repo, tag) = split_image_ref(image)?;
		// `None` leaves Podman's default (pause on), matching `docker commit`.
		let pause = opts.pause.unwrap_or(true);
		let mut path = commit_path(&container, repo, tag, pause);
		if let Some(message) = &opts.message {
			path.push_str(&format!("&comment={}", urlencoded(message)));
		}
		if let Some(author) = &opts.author {
			path.push_str(&format!("&author={}", urlencoded(author)));
		}
		for change in &opts.changes {
			path.push_str(&format!("&changes={}", urlencoded(change)));
		}
		self.client
			.post_empty_ok(&path)
			.await
			.map_err(ComposeError::Podman)?;
		tracing::info!("committed {container} to {repo}:{tag}");
		Ok(())
	}

	/// Export a service container's filesystem as a tar stream (`docker compose
	/// export`), to `output` or stdout. Streamed chunk-by-chunk so a large
	/// rootfs is never fully buffered in memory.
	pub async fn export(
		&self,
		file: &ComposeFile,
		service_name: &str,
		output: Option<PathBuf>,
		index: Option<u32>,
	) -> Result<()> {
		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into()))?;
		let container = self.replica_name_at(service_name, service, index)?;

		// Refuse to flood a terminal with a binary tar stream when no output file
		// is given, matching `docker export`.
		if refuse_tar_to_tty(output.is_none(), std::io::stdout().is_terminal()) {
			return Err(ComposeError::Unsupported(
				"refusing to write a tar archive to the terminal: pass -o FILE or redirect stdout"
					.into(),
			));
		}

		let path = format!("{API_PREFIX}/containers/{}/export", urlencoded(&container),);
		let resp = self
			.client
			.get_stream(&path)
			.await
			.map_err(ComposeError::Podman)?;

		// `-o -` streams to stdout (the coreutils/docker dash convention) rather
		// than creating a file literally named `-`.
		let to_stdout = output.is_none() || output.as_deref() == Some(std::path::Path::new("-"));
		let mut sink: Box<dyn Write> = if to_stdout {
			Box::new(std::io::stdout().lock())
		} else {
			let p = output.as_ref().unwrap();
			Box::new(std::fs::File::create(p).map_err(|e| ComposeError::IoPath {
				path: p.display().to_string(),
				source: e,
			})?)
		};
		let mut body = resp.into_body();
		while let Some(frame) = body.frame().await {
			let frame = frame.map_err(|e| ComposeError::Build(format!("export stream: {e}")))?;
			if let Ok(data) = frame.into_data() {
				if let Err(e) = sink.write_all(&data) {
					// A downstream reader that closed early (`podup export svc | head`)
					// surfaces as a broken pipe; stop quietly and successfully like any
					// well-behaved Unix tool instead of printing an error and exiting 1.
					if is_stdout_broken_pipe(to_stdout, &e) {
						return Ok(());
					}
					return Err(io_to_err(&output, e));
				}
			}
		}
		if let Err(e) = sink.flush() {
			if is_stdout_broken_pipe(to_stdout, &e) {
				return Ok(());
			}
			return Err(io_to_err(&output, e));
		}
		Ok(())
	}
}

/// Whether a write error is a broken pipe on the stdout sink — a downstream
/// reader closed early. A broken pipe never applies to a `-o FILE` sink, so it
/// only short-circuits to a clean exit when streaming to stdout. Pure so it is
/// unit-tested.
fn is_stdout_broken_pipe(to_stdout: bool, e: &std::io::Error) -> bool {
	to_stdout && e.kind() == std::io::ErrorKind::BrokenPipe
}

/// Map a write error to one that names the `-o` output path when present, so the
/// user learns which destination failed.
fn io_to_err(output: &Option<PathBuf>, e: std::io::Error) -> ComposeError {
	match output {
		Some(p) => ComposeError::IoPath {
			path: p.display().to_string(),
			source: e,
		},
		None => ComposeError::Io(e),
	}
}

/// Whether an `export` should be refused: true only when no output file was
/// given *and* stdout is a terminal. Pure so the guard is unit-tested.
fn refuse_tar_to_tty(no_output_file: bool, stdout_is_tty: bool) -> bool {
	no_output_file && stdout_is_tty
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;

/// End-to-end check that the destination path actually surfaces as
/// [`ComposeError::IoPath`] when the operator passes an output the file
/// system refuses to create — i.e. the production wiring from `Engine::export`
/// through `std::fs::File::create` into `IoPath`, not just the helper that
/// shapes the error. The bare `io_to_err` unit test above pins the wrapper;
/// this one pins the call site that triggers it.
#[cfg(unix)]
#[cfg(test)]
#[path = "export_iopath_tests.rs"]
mod export_iopath_tests;
