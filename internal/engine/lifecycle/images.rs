//! Deciding which image a service runs, and getting it there.
//!
//! Split out of `mod.rs` to keep that file within the source line limit.

use crate::compose::types::{ComposeFile, Service};
use crate::error::Result;

use super::Engine;

impl Engine {
	/// The image tag a service resolves to: its explicit `image:` when set,
	/// otherwise the tag its build produces (`build.tags[0]`, else the
	/// project-scoped `{project}-{service}:latest`).
	///
	/// This is the name `up` checks for presence and the name `down --rmi local`
	/// removes, so both must agree on it — they used to compute it separately.
	pub(super) fn service_image_tag(&self, name: &str, service: &Service) -> String {
		match &service.image {
			Some(image) => image.clone(),
			None => crate::engine::build::primary_build_tag(
				&self.project,
				name,
				None,
				service.build.as_ref().map(|b| b.tags()).unwrap_or(&[]),
			),
		}
	}

	/// Make the service's image available before its containers are created:
	/// build it, pull it, or leave the local one alone.
	pub(super) async fn acquire_service_image(
		&self,
		name: &str,
		service: &Service,
		file: &ComposeFile,
	) -> Result<()> {
		// `up --pull <policy>` overrides the per-service `pull_policy`; `--no-build`
		// suppresses building even for services with a `build:` section (they fall
		// back to pulling/using an existing image).
		let policy = self
			.pull_policy_override
			.as_deref()
			.or(service.pull_policy.as_deref())
			.unwrap_or("missing");
		// Build on `up` only when the service's image is not already there, which
		// is what docker compose does: `up` converges on the declared state and
		// `--build` is the flag that forces a rebuild.
		//
		// Building unconditionally was worse than redundant. The rebuild runs
		// *with* the cache, so it can resolve to an older layer chain and retag
		// the image backwards — silently undoing a `podup build --no-cache` that
		// just ran. `build --no-cache && up -d`, the ordinary deploy shape, would
		// start the previous image. It also made `--build` look like a no-op,
		// since the default already always built.
		//
		// `--build` is handled before this by an explicit `build_all`, so a forced
		// rebuild has already happened and the image is present by the time we get
		// here.
		let needs_build = if service.build.is_some() && !self.no_build {
			!self
				.image_present(&self.service_image_tag(name, service))
				.await
		} else {
			false
		};
		match (needs_build, policy) {
			(true, _) => {
				self.build_service(name, service, file, &crate::engine::BuildOptions::default())
					.await?
			}
			// A service with a `build:` whose image is already present needs no
			// pull either — the local tag is the declared state.
			(false, _) if service.build.is_some() => {}
			(false, "never") => {}
			// Under `missing`, an image the prefetch stage already saw on this
			// host needs no request at all. Skipping only what was observed in
			// this invocation keeps the decision as fresh as the one the pull
			// itself would have made.
			(false, _) if self.image_already_seen_present(service) => {}
			(false, _) => self.pull_image(service).await?,
		}
		Ok(())
	}

	/// Whether the prefetch stage observed this service's image present on the
	/// host during this invocation, making its pull a no-op worth skipping.
	///
	/// False for anything but a normalized `missing` policy: `always` and
	/// `newer` mean go to the registry, and widening this to them would bring
	/// back #1076, where a pull that failed was reported as success — libpod
	/// sends that failure as an in-band line on a 200, so no pull means no line
	/// to miss.
	///
	/// False for a service pinning `platform:`. The observation matched an image
	/// reference, which carries no architecture, so honouring it there could
	/// start the wrong variant.
	fn image_already_seen_present(&self, service: &Service) -> bool {
		if service.platform.is_some() {
			return false;
		}
		let raw_policy = self
			.pull_policy_override
			.as_deref()
			.or(service.pull_policy.as_deref());
		if crate::engine::build::libpod_pull_policy(raw_policy).unwrap_or("missing") != "missing" {
			return false;
		}
		let Some(image) = service.image.as_deref() else {
			return false;
		};
		self.images_seen_present
			.lock()
			.map(|seen| seen.contains(image))
			.unwrap_or(false)
	}
}

/// Tests for the pull decision above.
///
/// They drive a whole `up`, so they would sit as naturally in the lifecycle
/// suite — but that file was already at 487 of its 500 code lines, and adding
/// them there put it over. They belong next to the decision they pin anyway,
/// the same way `prefetch.rs` keeps its own.
#[cfg(test)]
#[cfg(unix)]
mod tests {
	use crate::engine::fake_podman;
	use crate::engine::Engine;

	fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
		Engine::with_base_dir(client, project.into(), std::env::temp_dir())
	}

	/// A libpod stand-in for a host that already has every image asked about — the
	/// warm state the pull-skip decision turns on. Shared by the three tests that
	/// exercise that decision, so they cannot drift apart on what "already here"
	/// means.
	fn present_image_engine(method: &str, target: &str) -> (u16, String) {
		if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
			(200, r#"{"Id":"sha256:cafe"}"#.to_string())
		} else if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else if method == "POST" && target.contains("/containers/create") {
			(200, "{}".to_string())
		} else if method == "POST" && target.contains("/start") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	}

	/// A warm `up` must not pull an image the host already has, once per service.
	///
	/// The prefetch stage checks presence once and returns without pulling, and then
	/// `acquire_service_image` pulled anyway, per service, under the effective
	/// `missing` policy — 42 of the 88 requests a 42-service warm `up` issued, and a
	/// `Pulling` line on the user's terminal for each. docker compose against the
	/// same engine prints none.
	///
	/// Nothing in the suite counted the pull requests an `up` issues, which is how
	/// that survived; this is that count.
	#[tokio::test]
	async fn warm_up_does_not_pull_an_image_the_host_already_has() {
		let fake = fake_podman::start(present_image_engine);
		let e = engine_with(fake.client(), "proj");

		let file = crate::parse_str(
			"services:\n  a:\n    image: shared\n  b:\n    image: shared\n  c:\n    image: shared\n",
		)
		.unwrap();

		e.up_with_options(&file, false, &[], &[], false, false, false)
			.await
			.expect("a warm up on a present image must succeed");

		let seen = fake.requests.lock().unwrap();
		let pulls = seen.iter().filter(|r| r.contains("/images/pull")).count();
		assert_eq!(
			pulls, 0,
			"three services sharing an image the host already has must pull it zero times, not once each: {seen:?}"
		);
	}

	/// The skip is bounded by the `missing` policy. `always` means go to the
	/// registry whatever is local, and widening the skip to it would bring back
	/// #1076: libpod reports a failed pull as an in-band line on a 200, so a pull
	/// that never happens is a failure line that can never be read.
	///
	/// The two services share one image on purpose, with different policies. With
	/// `always` alone the prefetch stage never records the image as present, so the
	/// skip could not fire whatever the policy check said, and the test would pass
	/// for the wrong reason — it did, until a mutation run showed it surviving the
	/// removal of the very guard it is named for. The `missing` service is what
	/// records the observation, which is the only state where that guard is the one
	/// thing standing between `always` and a skipped registry visit.
	#[tokio::test]
	async fn an_always_policy_still_pulls_an_image_another_service_saw_present() {
		let fake = fake_podman::start(present_image_engine);
		let e = engine_with(fake.client(), "proj");

		let file = crate::parse_str(
			"services:\n  a:\n    image: shared\n  b:\n    image: shared\n    pull_policy: always\n",
		)
		.unwrap();

		e.up_with_options(&file, false, &[], &[], false, false, false)
			.await
			.expect("an always-policy up must succeed");

		let seen = fake.requests.lock().unwrap();
		assert!(
			seen.iter().any(|r| r.contains("/images/pull")),
			"an always policy must still reach the registry even when a sibling service saw the image locally: {seen:?}"
		);
	}

	/// The skip never applies to a service pinning `platform:`. Presence is matched
	/// on the image reference, which carries no architecture, so honouring an
	/// observation there could start the wrong variant.
	#[tokio::test]
	async fn a_platform_pinned_service_still_pulls_an_image_the_host_already_has() {
		let fake = fake_podman::start(present_image_engine);
		let e = engine_with(fake.client(), "proj");

		let file = crate::parse_str(
			"services:\n  a:\n    image: shared\n  b:\n    image: shared\n    platform: linux/arm64\n",
		)
		.unwrap();

		e.up_with_options(&file, false, &[], &[], false, false, false)
			.await
			.expect("a platform-pinned up must succeed");

		let seen = fake.requests.lock().unwrap();
		assert!(
			seen.iter().any(|r| r.contains("/images/pull")),
			"a platform-pinned service must still pull: a reference match says nothing about the architecture that is local: {seen:?}"
		);
	}
}
