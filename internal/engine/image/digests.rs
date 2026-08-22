//! `config --resolve-image-digests`: pin each service image to its registry
//! digest.
//!
//! Like `ls`, this is project-agnostic — it needs only a [`Client`] to inspect
//! images, not a full [`Engine`](crate::engine::Engine) — so it lives as a free function.

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
/// one dependency level over three rounds with **zero drops** — recorded in
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
/// Inspects each image with at most [`MAX_RESOLVE_CONCURRENCY`] requests in
/// flight, so a hundred-service project pays ceiling(S/N)+1 round-trips
/// against libpod instead of the previous S+1. The structural speedup is
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

	let outcomes: Vec<(String, ResolveOutcome)> =
		futures_util::stream::iter(inputs.into_iter().map(|(name, image)| async move {
			let path = format!("{API_PREFIX}/images/{}/json", urlencoded(&image));
			match client.get_json::<ImageInspect>(&path).await {
				Ok(info) => match info.repo_digests.into_iter().next() {
					Some(digest) => (name, ResolveOutcome::Pinned(digest)),
					None => {
						tracing::warn!(
							"config --resolve-image-digests: no registry digest for {image} \
								 (service {name}); left unchanged"
						);
						(name, ResolveOutcome::NoDigest)
					}
				},
				Err(e) => (name, ResolveOutcome::Failed(image, e)),
			}
		}))
		.buffer_unordered(MAX_RESOLVE_CONCURRENCY)
		.collect()
		.await;

	// Surface the first failing service in **input order** and abort before any
	// mutation, mirroring the pre-concurrency serial behaviour. A backend
	// failure (unreachable socket, HTTP 500) must be a hard error: emitting the
	// original UNPINNED config with exit 0 would let a script that relies on
	// digest pinning silently get unpinned images. A genuinely-absent image
	// (404) is reported the same way: the user asked to pin and we could not.
	for (name, outcome) in &outcomes {
		if let ResolveOutcome::Failed(image, e) = outcome {
			return Err(ComposeError::Build(format!(
				"config --resolve-image-digests: cannot inspect {image} (service {name}): {e}"
			)));
		}
	}

	// All good (or all warn-only); apply the pinned digests in input order so
	// the resulting file is stable across re-runs regardless of which inspect
	// call happened to resolve first.
	for (name, outcome) in outcomes {
		if let ResolveOutcome::Pinned(digest) = outcome {
			if let Some(svc) = out.services.get_mut(&name) {
				svc.image = Some(digest);
			}
		}
	}

	Ok(out)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::compose::parse_str;
	use crate::engine::fake_podman;

	/// Build a compose file with one service per `name:image` pair, in the
	/// order given. Services without an `image:` are skipped.
	fn file_with(services: &[(&str, Option<&str>)]) -> ComposeFile {
		let mut yaml = String::from("services:\n");
		for (name, image) in services {
			match image {
				Some(img) => yaml.push_str(&format!("  {name}:\n    image: {img}\n")),
				None => yaml.push_str(&format!(
					"  {name}:\n    command: [\"sleep\", \"infinity\"]\n"
				)),
			}
		}
		parse_str(&yaml).expect("parse compose fixture")
	}

	/// Build the body of a `GET /libpod/images/{name}/json` response that
	/// carries one registry digest. `image` must match the path component in
	/// `target`.
	fn inspect_body(image: &str, digest_suffix: &str) -> String {
		format!(
			r#"{{"Id":"sha256:d{ds}","RepoDigests":["{image}@sha256:d{ds}"],"Size":1,"Created":"2026-01-01T00:00:00Z"}}"#,
			ds = digest_suffix,
		)
	}

	#[tokio::test]
	async fn pins_every_service_image_to_its_registry_digest() {
		let file = file_with(&[
			("a", Some("alpine-1")),
			("b", Some("alpine-2")),
			("c", Some("alpine-3")),
		]);
		let fake = fake_podman::start(|_method, target| {
			let (status, body) = if target.contains("alpine-1") {
				(200, inspect_body("alpine-1", "1"))
			} else if target.contains("alpine-2") {
				(200, inspect_body("alpine-2", "2"))
			} else if target.contains("alpine-3") {
				(200, inspect_body("alpine-3", "3"))
			} else {
				(404, r#"{"message":"not used"}"#.to_string())
			};
			(status, body)
		});
		let client = fake.client();

		let resolved = resolve_image_digests(&client, &file).await.unwrap();
		let pin = |n: &str| resolved.services[n].image.as_deref().unwrap().to_string();
		assert_eq!(pin("a"), "alpine-1@sha256:d1");
		assert_eq!(pin("b"), "alpine-2@sha256:d2");
		assert_eq!(pin("c"), "alpine-3@sha256:d3");
		// Every service that declared an image was inspected.
		assert_eq!(
			fake.requests.lock().unwrap().len(),
			3,
			"each service with an image: triggers exactly one inspect"
		);
	}

	#[tokio::test]
	async fn service_without_image_is_not_inspected() {
		let file = file_with(&[
			("a", Some("alpine-1")),
			("noimg", None),
			("b", Some("alpine-2")),
		]);
		let fake = fake_podman::start(|_, target| {
			let (status, body) = if target.contains("alpine-1") {
				(200, inspect_body("alpine-1", "1"))
			} else if target.contains("alpine-2") {
				(200, inspect_body("alpine-2", "2"))
			} else {
				(404, r#"{"message":"not used"}"#.to_string())
			};
			(status, body)
		});
		let client = fake.client();

		let resolved = resolve_image_digests(&client, &file).await.unwrap();
		// The no-image service stays as it was (no `image:` key, nothing to
		// pin).
		assert!(resolved.services["noimg"].image.is_none());
		// The two services with images are pinned; the no-image one was
		// skipped at the fan-out, so only two requests reached the fake.
		assert_eq!(fake.requests.lock().unwrap().len(), 2);
	}

	#[tokio::test]
	async fn image_without_digest_warns_and_leaves_image_unchanged() {
		let file = file_with(&[("local", Some("built-locally"))]);
		// The fake responds with an inspect body that carries no
		// `RepoDigests` — the "locally built image" case.
		let fake = fake_podman::start(|_, _| {
			(
				200,
				r#"{"Id":"sha256:d0","RepoDigests":[],"Size":1,"Created":"2026-01-01T00:00:00Z"}"#
					.to_string(),
			)
		});
		let client = fake.client();

		let resolved = resolve_image_digests(&client, &file).await.unwrap();
		// The image was left untouched: the user is told via warn!, the
		// returned file still names the local tag.
		assert_eq!(
			resolved.services["local"].image.as_deref(),
			Some("built-locally"),
		);
	}

	#[tokio::test]
	async fn first_inspect_failure_aborts_with_the_offending_service_named() {
		// `b` is the second service in file order and is the one whose
		// inspect fails. The fan-out completes both `a` and `c` before
		// surfacing the error; the resolver must name `b` specifically.
		let file = file_with(&[
			("a", Some("alpine-1")),
			("b", Some("alpine-2")),
			("c", Some("alpine-3")),
		]);
		let fake = fake_podman::start(|_, target| {
			if target.contains("alpine-2") {
				(500, r#"{"message":"backend exploded"}"#.to_string())
			} else if target.contains("alpine-1") {
				(200, inspect_body("alpine-1", "1"))
			} else if target.contains("alpine-3") {
				(200, inspect_body("alpine-3", "3"))
			} else {
				(404, r#"{"message":"not used"}"#.to_string())
			}
		});
		let client = fake.client();

		let err = resolve_image_digests(&client, &file).await.unwrap_err();
		// `matches!` on a distinctive fragment, not `is_err()` — the test
		// has to fail when the wrong service is named.
		let msg = match err {
			ComposeError::Build(m) => m,
			other => panic!("expected ComposeError::Build, got {other:?}"),
		};
		assert!(
			msg.contains("(service b)"),
			"message must name service b: {msg}"
		);
		assert!(
			msg.contains("alpine-2"),
			"message must name the image: {msg}"
		);
		// The 500 from libpod is propagated verbatim into the message; the
		// distinctive fragment "backend exploded" lets us pin the wiring
		// against a silent regression that swallowed the cause.
		assert!(
			msg.contains("backend exploded"),
			"underlying libpod error must be surfaced: {msg}"
		);
	}

	#[tokio::test]
	async fn missing_image_404_is_a_hard_error_naming_the_service() {
		// The pre-concurrency serial behaviour treated a 404 the same as a
		// transport failure: the user asked to pin, we could not, so we
		// refuse to emit a silently-unpinned file. The fan-out must
		// preserve that.
		let file = file_with(&[("missing", Some("never-pulled"))]);
		let fake = fake_podman::start(|_, _| (404, r#"{"message":"no such image"}"#.to_string()));
		let client = fake.client();

		let err = resolve_image_digests(&client, &file).await.unwrap_err();
		let msg = match err {
			ComposeError::Build(m) => m,
			other => panic!("expected ComposeError::Build, got {other:?}"),
		};
		assert!(
			msg.contains("(service missing)"),
			"message must name service: {msg}"
		);
		assert!(
			msg.contains("never-pulled"),
			"message must name the image: {msg}"
		);
		assert!(
			msg.contains("no such image"),
			"underlying 404 reason must be surfaced: {msg}"
		);
	}

	#[tokio::test]
	async fn first_failing_service_in_input_order_wins_even_when_others_finish_first() {
		// `b` fails, `a` is the first service in input order and resolves
		// cleanly. Under fan-out completion order is not file order; the
		// test pins that the error reported is still the one for the file's
		// order, and the first service's outcome is irrelevant to error
		// selection.
		let file = file_with(&[
			("a", Some("alpine-1")),
			("b", Some("alpine-2")),
			("c", Some("alpine-3")),
		]);
		let fake = fake_podman::start(|_, target| {
			let body = if target.contains("alpine-2") {
				return (500, r#"{"message":"backend exploded"}"#.to_string());
			} else if target.contains("alpine-1") {
				inspect_body("alpine-1", "1")
			} else if target.contains("alpine-3") {
				inspect_body("alpine-3", "3")
			} else {
				return (404, r#"{"message":"not used"}"#.to_string());
			};
			(200, body)
		});
		let client = fake.client();

		let err = resolve_image_digests(&client, &file).await.unwrap_err();
		let msg = match err {
			ComposeError::Build(m) => m,
			other => panic!("expected ComposeError::Build, got {other:?}"),
		};
		assert!(
			msg.contains("(service b)"),
			"must name the failing service, not one that succeeded: {msg}"
		);
	}
}
