//! Best-effort image prefetch ahead of the per-level `up` walk.
//!
//! Without this stage each service's image is pulled inside `up_one_service`,
//! gated behind the dependency-level barrier: on a cold start, a level-2
//! service's image acquisition does not even begin until every level-1 service
//! is fully up. This stage collects every image the upcoming `up`/`create` pass will
//! pull and warms the local Podman cache for all of them up front,
//! concurrently, before the first level barrier — instead of one at a time as
//! each level's services reach their turn.
//!
//! Best-effort only: a prefetch miss is logged at debug and otherwise
//! swallowed. `up_one_service`'s own pull call is unchanged and remains the
//! sole source of a real pull failure — this stage can only make `up` faster,
//! never change whether it succeeds.

use std::collections::{HashMap, HashSet};

use crate::compose::types::{ComposeFile, Service};
use crate::engine::build::libpod_pull_policy;

use super::parallel::join_bounded;
use super::Engine;

impl Engine {
	/// Warm the local image cache for every service the upcoming `up`/`create`
	/// pass will pull, before the per-level walk begins.
	///
	/// Mirrors the pull-policy resolution `up_one_service` applies at its own
	/// pull site (`--pull` override, else the service's `pull_policy`, else
	/// `missing`): a service building an image (and not overridden by
	/// `--no-build`), or one whose effective policy is `never`, has nothing to
	/// prefetch. Deduplicates by image reference, so many services sharing one
	/// image pull it once instead of once per service, and dispatches the
	/// resulting pulls with the same bounded concurrency the level fan-out
	/// uses. Never fails: `up_one_service`'s own pull remains authoritative, so
	/// a prefetch miss here just means that later call does the work instead,
	/// exactly as it would without this stage.
	pub(super) async fn prefetch_images(
		&self,
		file: &ComposeFile,
		enabled: &HashSet<String>,
		target_set: &Option<HashSet<String>>,
	) {
		// One representative service per unique image reference is enough to
		// issue the pull — this is what dedupes 50 services on one image down
		// to a single request instead of 50.
		let mut by_image: HashMap<&str, &Service> = HashMap::new();
		for (name, service) in &file.services {
			if !enabled.contains(name) {
				continue;
			}
			if let Some(set) = target_set {
				if !set.contains(name) {
					continue;
				}
			}
			// A service with an active build lane builds its image; it never
			// pulls, so it has nothing to prefetch.
			if service.build.is_some() && !self.no_build {
				continue;
			}
			let Some(image) = service.image.as_deref() else {
				continue;
			};
			let raw_policy = self
				.pull_policy_override
				.as_deref()
				.or(service.pull_policy.as_deref());
			if libpod_pull_policy(raw_policy).unwrap_or("missing") == "never" {
				continue;
			}
			by_image.entry(image).or_insert(service);
		}

		let futs = by_image.into_values().map(|service| async move {
			let image = service.image.as_deref().unwrap_or_default();
			let raw_policy = self
				.pull_policy_override
				.as_deref()
				.or(service.pull_policy.as_deref());
			let policy = libpod_pull_policy(raw_policy).unwrap_or("missing");
			// `missing` (and its aliases, already normalized by
			// `libpod_pull_policy`) only pulls when the image is absent —
			// checking first turns a warm cache into a cheap presence check
			// instead of a redundant pull request. `always`/`newer` mean to
			// hit the registry regardless, so skip the check and prefetch
			// unconditionally: that request is a pure win, since
			// `up_one_service` would have made it anyway, just later.
			if policy == "missing" && self.image_present(image).await {
				// Record what this check just observed, so the per-service pull
				// site does not repeat it: without this the stage returns having
				// learned the image is here, and `acquire_service_image` pulls it
				// once per service anyway — 42 of the 88 requests a 42-service
				// warm `up` used to issue.
				//
				// Not recorded for a service pinning `platform:`. Presence is
				// matched on the reference, which says nothing about which
				// architecture variant is local, so a hit here could stand in for
				// the wrong image.
				if service.platform.is_none() {
					if let Ok(mut seen) = self.images_seen_present.lock() {
						seen.insert(image.to_string());
					}
				}
				return;
			}
			if let Err(e) = self.pull_image(service).await {
				tracing::debug!("prefetch miss for {image}: {e}");
			}
		});

		join_bounded(futs).await;
	}
}

#[cfg(test)]
mod tests {
	#[cfg(unix)]
	use std::collections::HashSet;

	#[cfg(unix)]
	use crate::engine::fake_podman;
	#[cfg(unix)]
	use crate::engine::Engine;

	#[cfg(unix)]
	fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
		Engine::with_base_dir(client, project.into(), std::env::temp_dir())
	}

	/// Two services on the same image pull it once, and a `never`-policy
	/// service plus a `build:` service are excluded entirely — the image
	/// reference never appears in a request at all.
	#[tokio::test]
	#[cfg(unix)]
	async fn prefetch_dedupes_shared_image_and_skips_never_and_build_services() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				(200, String::new())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = engine_with(fake.client(), "proj");

		let file = crate::parse_str(
			"services:\n  a:\n    image: shared\n  b:\n    image: shared\n  c:\n    image: skip-me\n    pull_policy: never\n  d:\n    image: build-me\n    build:\n      context: .\n",
		)
		.unwrap();
		let enabled: HashSet<String> = file.services.keys().cloned().collect();

		e.prefetch_images(&file, &enabled, &None).await;

		let seen = fake.requests.lock().unwrap();
		let shared_pulls = seen
			.iter()
			.filter(|r| r.contains("/images/pull") && r.contains("reference=shared"))
			.count();
		assert_eq!(
			shared_pulls, 1,
			"two services sharing one image must pull it once: {seen:?}"
		);
		assert!(
			!seen.iter().any(|r| r.contains("skip-me")),
			"a never-policy service must not be prefetched: {seen:?}"
		);
		assert!(
			!seen.iter().any(|r| r.contains("build-me")),
			"a service with a build: section must not be prefetched: {seen:?}"
		);
	}

	/// A service outside the `up --target` set (or disabled by profile) is not
	/// prefetched, matching what `up_one_service` would skip anyway.
	#[tokio::test]
	#[cfg(unix)]
	async fn prefetch_skips_services_outside_the_target_set() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				(200, String::new())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = engine_with(fake.client(), "proj");

		let file =
			crate::parse_str("services:\n  web:\n    image: img-web\n  db:\n    image: img-db\n")
				.unwrap();
		let enabled: HashSet<String> = file.services.keys().cloned().collect();
		let target_set: Option<HashSet<String>> = Some(["web".to_string()].into_iter().collect());

		e.prefetch_images(&file, &enabled, &target_set).await;

		let seen = fake.requests.lock().unwrap();
		assert!(
			seen.iter()
				.any(|r| r.contains("/images/pull") && r.contains("reference=img-web")),
			"the targeted service's image must be prefetched: {seen:?}"
		);
		assert!(
			!seen.iter().any(|r| r.contains("img-db")),
			"a service outside the target set must not be prefetched: {seen:?}"
		);
	}
}
