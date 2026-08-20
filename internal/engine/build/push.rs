//! Image push to a registry (docker compose `push`).

use std::time::Duration;

use futures_util::{Stream, StreamExt};
use tracing::{info, warn};

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::types::image::ImagePullProgress;
use crate::libpod::{urlencoded, PodmanError, API_PREFIX};

use super::super::Engine;

/// Maximum time to wait for the next progress line from a push body stream
/// before treating the registry as unresponsive.
///
/// The client `READ_TIMEOUT` only bounds the request head, not this streamed
/// body, so without a per-line deadline a push to an unreachable/wedged registry
/// would block indefinitely while draining the response. Generous so a slow but
/// progressing upload is never aborted.
const PUSH_STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Drain a push progress stream, surfacing a mid-stream `error` line and bounding
/// each read by `stall` so an unresponsive registry fails with a clear timeout
/// instead of hanging. `quiet` (`-q/--quiet`) suppresses the per-line progress
/// and the final "pushed" line. Generic over the stream so it is unit-tested
/// without a live socket.
async fn drain_push_stream<S>(
	mut stream: S,
	image: &str,
	quiet: bool,
	stall: Duration,
) -> Result<()>
where
	S: Stream<Item = std::result::Result<ImagePullProgress, PodmanError>> + Unpin,
{
	let mut stream_error: Option<String> = None;
	loop {
		match tokio::time::timeout(stall, stream.next()).await {
			Ok(Some(Ok(progress))) => {
				if !progress.stream.is_empty() && !quiet {
					info!("{}", progress.stream.trim_end());
				}
				if !progress.error.is_empty() {
					stream_error = Some(progress.error.clone());
				}
			}
			Ok(Some(Err(e))) => stream_error = Some(e.to_string()),
			Ok(None) => break,
			Err(_elapsed) => {
				return Err(ComposeError::Build(format!(
					"push {image}: no progress from the registry for {}s; aborting \
					 (registry unreachable or unresponsive)",
					stall.as_secs()
				)));
			}
		}
	}
	match stream_error {
		Some(err) => Err(ComposeError::Build(format!("push {image}: {err}"))),
		None => {
			if quiet {
				tracing::debug!("pushed {image}");
			} else {
				crate::ui::progress_line("Image", image, "Pushed");
			}
			Ok(())
		}
	}
}

/// Options for [`Engine::push`], mirroring `docker compose push` (plus a Podman
/// `--tls-verify` escape hatch for insecure/local registries).
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`PushOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PushOptions {
	/// Continue with the remaining services after a push fails.
	pub ignore_failures: bool,
	/// Override TLS verification of the registry. `None` leaves Podman's default
	/// (verify on); `Some(false)` allows an insecure/HTTP registry.
	pub tls_verify: Option<bool>,
}

impl PushOptions {
	/// Every `docker compose push` flag, in CLI order. A constructor rather than
	/// a struct literal because the type is `#[non_exhaustive]`, so the next
	/// flag to land is not a breaking change for anyone building one.
	pub fn new(ignore_failures: bool, tls_verify: Option<bool>) -> Self {
		Self {
			ignore_failures,
			tls_verify,
		}
	}

	/// Continue with the remaining services after a push fails,
	/// `--ignore-push-failures`. Builder-style.
	#[must_use]
	pub fn with_ignore_failures(mut self, ignore_failures: bool) -> Self {
		self.ignore_failures = ignore_failures;
		self
	}

	/// Override TLS verification of the registry, `--tls-verify` /
	/// `--tls-verify=false`. `None` leaves Podman's default (verify on).
	/// Builder-style.
	#[must_use]
	pub fn with_tls_verify(mut self, tls_verify: Option<bool>) -> Self {
		self.tls_verify = tls_verify;
		self
	}
}

impl Engine {
	/// Push each service's image to its registry. Services without an `image:`
	/// (build-only or imageless) are skipped. Registry credentials come from
	/// Podman's auth file (`podman login`), so no auth handling is needed here.
	pub async fn push(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		opts: PushOptions,
	) -> Result<()> {
		self.push_with_quiet(file, target_services, opts, false)
			.await
	}

	/// Push each service's image like [`Engine::push`], with `quiet` (`-q/--quiet`)
	/// suppressing the per-image progress output. Kept off the frozen
	/// [`PushOptions`] struct so the published library API stays stable across minors.
	pub async fn push_with_quiet(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		opts: PushOptions,
		quiet: bool,
	) -> Result<()> {
		for svc in target_services {
			if !file.services.contains_key(svc) {
				return Err(ComposeError::ServiceNotFound(svc.clone()));
			}
		}

		for (name, service) in &file.services {
			if !target_services.is_empty() && !target_services.iter().any(|t| t == name) {
				continue;
			}
			let Some(image) = service.image.as_deref() else {
				tracing::debug!("{name}: no image to push, skipping");
				continue;
			};
			self.try_push(image, &opts, quiet).await?;
			// `build.tags` is locally retagged by `apply_extra_tags` after the
			// build. Push each one too — the registry would otherwise end up
			// with only the primary `image`, and the other refs a user expects
			// to deploy would never be published (#1476).
			if let Some(build) = &service.build {
				for extra in build.tags() {
					if extra == image {
						continue;
					}
					self.try_push(extra, &opts, quiet).await?;
				}
			}
		}
		Ok(())
	}

	/// Push a single image ref, applying `--ignore-push-failures` semantics:
	/// warn-and-continue when the flag is set, otherwise surface the error so
	/// the caller aborts the whole push. Shared by the primary and the extra
	/// `build.tags` loop in [`Engine::push_with_quiet`] so the two cannot drift
	/// on how a per-image failure is treated.
	async fn try_push(&self, image: &str, opts: &PushOptions, quiet: bool) -> Result<()> {
		match self.push_image(image, opts, quiet).await {
			Ok(()) => Ok(()),
			Err(e) if opts.ignore_failures => {
				warn!("push {image} failed (ignored): {e}");
				Ok(())
			}
			Err(e) => Err(e),
		}
	}

	/// Push a single image ref and drain its progress stream, surfacing a
	/// mid-stream `error` line as a failure.
	///
	/// The two user-facing lines go through `ui::progress_line`, not `tracing`.
	/// They were `info!` until #1248, and the CLI floors tracing at `warn`
	/// everywhere except `watch` — so a successful `podup push` wrote **zero
	/// bytes** to stdout and stderr while the image really did land in the
	/// registry, and `--quiet` suppressed output that never appeared. Routing
	/// through the progress layer also honours `PROGRESS_ENABLED`, which is the
	/// switch an embedder actually controls; a tracing line is gated only by the
	/// log floor, which it does not.
	async fn push_image(&self, image: &str, opts: &PushOptions, quiet: bool) -> Result<()> {
		if quiet {
			tracing::debug!("pushing {image}");
		} else {
			crate::ui::progress_line("Image", image, "Pushing");
		}
		let mut query = format!("destination={}", urlencoded(image));
		if let Some(tls) = opts.tls_verify {
			query.push_str(&format!("&tlsVerify={tls}"));
		}
		let path = format!("{API_PREFIX}/images/{}/push?{query}", urlencoded(image));

		let resp = self
			.client
			.post_empty_stream(&path)
			.await
			.map_err(ComposeError::Podman)?;
		let stream = crate::libpod::parse_json_lines::<ImagePullProgress>(resp.into_body());
		drain_push_stream(stream, image, quiet, PUSH_STALL_TIMEOUT).await
	}
}

#[cfg(test)]
mod tests {
	use super::{drain_push_stream, ImagePullProgress};
	use crate::error::ComposeError;
	use crate::libpod::PodmanError;
	use futures_util::StreamExt;
	use std::time::Duration;

	fn progress(stream: &str, error: &str) -> ImagePullProgress {
		ImagePullProgress {
			stream: stream.to_string(),
			error: error.to_string(),
		}
	}

	#[tokio::test]
	async fn drain_ok_when_stream_completes_cleanly() {
		let items = vec![Ok(progress("pushing", "")), Ok(progress("done", ""))];
		let stream = futures_util::stream::iter(items);
		drain_push_stream(stream, "img", false, Duration::from_secs(5))
			.await
			.unwrap();
	}

	#[tokio::test]
	async fn drain_surfaces_mid_stream_error_line() {
		let items = vec![Ok(progress("", "denied: unauthorized"))];
		let stream = futures_util::stream::iter(items);
		let err = drain_push_stream(stream, "img", false, Duration::from_secs(5))
			.await
			.unwrap_err();
		assert!(matches!(err, ComposeError::Build(m) if m.contains("denied: unauthorized")));
	}

	#[tokio::test]
	async fn drain_times_out_on_an_unresponsive_stream() {
		// A stream that yields one line then never another stands in for a registry
		// that accepts the request then stalls — the per-line deadline must fire.
		let first = futures_util::stream::iter(vec![Ok(progress("pushing", ""))]);
		let stream = first.chain(futures_util::stream::pending::<
			std::result::Result<ImagePullProgress, PodmanError>,
		>());
		let err = drain_push_stream(stream, "img", false, Duration::from_millis(20))
			.await
			.unwrap_err();
		assert!(matches!(err, ComposeError::Build(m) if m.contains("no progress")));
	}
}
