//! `config --resolve-image-digests`: pin each service image to its registry
//! digest.
//!
//! Like `ls`, this is project-agnostic: it needs only a [`Client`] to inspect
//! images, not a full [`Engine`](crate::engine::Engine), so it lives as a free function.

use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::types::image::ImageInspect;
use crate::libpod::{urlencoded, Client, PodmanError, API_PREFIX};

/// Upper bound on the number of image-inspect requests [`resolve_image_digests`]
/// has in flight at once.
///
/// Why this number, not a guess: the integration suite's `--test-threads`
/// ceiling (#1322) and the engine-side fan-out measured in the same context are
/// **different problems**, and the suite's threshold does not transfer. On the
/// same plain-KVM guest that produced the suite's `IncompleteMessage`
/// dose-response table, a single `up` was driven through 2/5/10/20 services in
/// one dependency level over three rounds with **zero drops**, recorded in
/// `ai-context/context/podup/index.md` as the reason the suite's threshold
/// "is not the path at risk". Twenty concurrent is the measured-clean
/// ceiling; this cap sits a comfortable margin below it, conservative with
/// evidence behind it.
///
/// Shared so the remaining engine-side fan-out loops (#1519, the thirteen
/// teardown loops in `lifecycle/parallel.rs` and friends) can adopt the same
/// value rather than each inventing its own. The visibility is `pub(crate)`
/// for that reason; nothing outside the crate needs to know.
pub(crate) const MAX_RESOLVE_CONCURRENCY: usize = 8;

/// One image-inspect call's outcome, distilled for the post-fan-out pass.
enum ResolveOutcome {
	/// A registry digest was returned; rewrite `svc.image` to it.
	Pinned(String),
	/// The image exists but has no registry digest (built locally, never
	/// pulled); warn-and-skip like the serial loop.
	NoDigest,
	/// The call failed. The error is held (with the image reference) and
	/// reported by [`resolve_image_digests`] in **input order**, so the
	/// first failing service in the file wins regardless of which future
	/// happened to resolve first.
	Failed(String, PodmanError),
}

/// Return a copy of `file` with every service `image:` rewritten to its registry
/// digest (`repo@sha256:...`), matching `docker compose config
/// --resolve-image-digests`. An image with no registry digest in local storage
/// (e.g. built locally, or never pulled) is left unchanged with a warning.
///
/// Inspects each image with at most `N` requests in flight, where `N` is a
/// small fixed cap, so a project of `S` services pays ceiling(S/N)+1
/// round-trips against libpod instead of the previous S+1. The structural speedup is
/// "one round-trip per cap-quanta of work", not a measured wall-clock
/// number. Error and ordering behaviour match the serial loop byte-for-byte:
/// a backend failure (unreachable socket, HTTP 500, 404 on a missing image)
/// is a hard error that names the offending service, and the first such
/// error in file order wins.
pub async fn resolve_image_digests(client: &Client, file: &ComposeFile) -> Result<ComposeFile> {
	let mut out = file.clone();

	// Snapshot every (name, image) pair in file order. The fan-out below
	// produces results out of completion order, but the error pass and the
	// mutation pass both walk `inputs` in order, so the first failing service
	// in the file is the one reported and the final state of `out.services`
	// matches the serial loop's behaviour.
	let inputs: Vec<(String, String)> = out
		.services
		.iter()
		.filter_map(|(name, svc)| svc.image.clone().map(|image| (name.clone(), image)))
		.collect();

	// The index travels with each task. `buffer_unordered` yields in COMPLETION
	// order, so without carrying the input position there is nothing to sort
	// back to - and "the first failing service in input order" below would
	// really mean "whichever inspect happened to fail first", which is a
	// different message on every run against the same file.
	let mut outcomes: Vec<(usize, String, ResolveOutcome)> =
		futures_util::stream::iter(inputs.into_iter().enumerate().map(
			|(i, (name, image))| async move {
				let path = format!("{API_PREFIX}/images/{}/json", urlencoded(&image));
				match client.get_json::<ImageInspect>(&path).await {
					Ok(info) => match info.repo_digests.into_iter().next() {
						Some(digest) => (i, name, ResolveOutcome::Pinned(digest)),
						None => {
							tracing::warn!(
								"config --resolve-image-digests: no registry digest for {image} \
								 (service {name}); left unchanged"
							);
							(i, name, ResolveOutcome::NoDigest)
						}
					},
					Err(e) => (i, name, ResolveOutcome::Failed(image, e)),
				}
			},
		))
		.buffer_unordered(MAX_RESOLVE_CONCURRENCY)
		.collect()
		.await;

	// Back into input order. This is what makes the error below deterministic.
	outcomes.sort_by_key(|(i, _, _)| *i);

	// Surface the first failing service in **input order** and abort before any
	// mutation, mirroring the pre-concurrency serial behaviour. A backend
	// failure (unreachable socket, HTTP 500) must be a hard error: emitting the
	// original UNPINNED config with exit 0 would let a script that relies on
	// digest pinning silently get unpinned images. A genuinely-absent image
	// (404) is reported the same way: the user asked to pin and we could not.
	for (_, name, outcome) in &outcomes {
		if let ResolveOutcome::Failed(image, e) = outcome {
			return Err(ComposeError::Build(format!(
				"config --resolve-image-digests: cannot inspect {image} (service {name}): {e}"
			)));
		}
	}

	// All good (or all warn-only); apply the pinned digests in input order so
	// the resulting file is stable across re-runs regardless of which inspect
	// call happened to resolve first.
	for (_, name, outcome) in outcomes {
		if let ResolveOutcome::Pinned(digest) = outcome {
			if let Some(svc) = out.services.get_mut(&name) {
				svc.image = Some(digest);
			}
		}
	}

	Ok(out)
}

// `fake_podman` binds a Unix domain socket, so these only build where that
// exists. `export_iopath_tests` in export.rs is gated the same way and for
// the same reason.
#[cfg(unix)]
#[cfg(test)]
#[path = "digests_tests.rs"]
mod tests;
