//! Image pull from a registry (the non-build half of image acquisition).

use futures_util::StreamExt;
use tracing::{debug, warn};

use std::collections::{HashMap, HashSet};

use crate::compose::types::{ComposeFile, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::image::ImagePullProgress;
use crate::libpod::{urlencoded, API_PREFIX};

use super::super::Engine;

/// Options for [`Engine::pull_services_with_options`], mirroring `docker
/// compose pull` flags. The `--policy` override is carried on the engine (see
/// [`Engine::with_up_overrides`]), not here.
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`PullOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Default)]
#[non_exhaustive]
pub struct PullOptions {
	/// Warn and continue instead of aborting on the first failure,
	/// `--ignore-pull-failures`.
	pub ignore_failures: bool,
	/// Also pull each named service's transitive `depends_on`, `--include-deps`.
	pub include_deps: bool,
}

impl PullOptions {
	/// Every `docker compose pull` flag, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next flag to land is not a breaking change for anyone building one.
	pub fn new(ignore_failures: bool, include_deps: bool) -> Self {
		Self {
			ignore_failures,
			include_deps,
		}
	}

	/// Warn and continue instead of aborting on the first failure,
	/// `--ignore-pull-failures`. Builder-style.
	#[must_use]
	pub fn with_ignore_failures(mut self, ignore_failures: bool) -> Self {
		self.ignore_failures = ignore_failures;
		self
	}

	/// Also pull each named service's transitive `depends_on`, `--include-deps`.
	/// Builder-style.
	#[must_use]
	pub fn with_include_deps(mut self, include_deps: bool) -> Self {
		self.include_deps = include_deps;
		self
	}
}

/// Upper bound on how many distinct images a standalone `pull` fetches
/// concurrently. Mirrors the lifecycle level fan-out's own concurrency cap: a
/// compose file with many distinct images must not open an unbounded number
/// of simultaneous pull streams against the Podman socket.
const MAX_PULL_CONCURRENCY: usize = 16;

/// Run `futs` concurrently, capped at `limit` in flight at once. Unlike the
/// lifecycle fan-out's `join_bounded`, callers here have no use for
/// input-order results (the outcomes are reduced into an image-keyed map
/// right after), so this stays a plain bounded join.
async fn bounded_join_all<F, T>(futs: impl IntoIterator<Item = F>, limit: usize) -> Vec<T>
where
	F: std::future::Future<Output = T>,
{
	futures_util::stream::iter(futs)
		.buffer_unordered(limit)
		.collect()
		.await
}

impl Engine {
	/// Pull images for all services that declare an `image:` key, concurrently.
	pub async fn pull(&self, file: &ComposeFile) -> Result<()> {
		self.pull_services(file, &[]).await
	}

	/// Pull images for the named services (or every service when `services` is
	/// empty), matching `docker compose pull [SERVICE...]`.
	pub async fn pull_services(&self, file: &ComposeFile, services: &[String]) -> Result<()> {
		self.pull_services_with_options(file, services, PullOptions::default())
			.await
	}

	/// Pull service images with `docker compose pull` options:
	/// `--include-deps` (also pull each named service's transitive
	/// `depends_on`) and `--ignore-pull-failures` (warn and continue instead of
	/// aborting on the first failure). The `--policy` override is applied via
	/// the engine's pull-policy override (see [`Engine::with_up_overrides`]).
	///
	/// Services that agree on every field that shapes the actual pull
	/// request — the image reference, the *resolved* pull policy (override
	/// applied), and the platform — pull it once, not once per service. Two
	/// services naming the same image but differing on the resolved policy
	/// or the platform each get their own pull, so per-service intent (e.g.
	/// one `never`, one `always`) is never silently collapsed onto whichever
	/// service happens to come first in the file. The actual pull is
	/// deduplicated by that key, dispatched with bounded concurrency, and
	/// each service still gets its own present/error report derived from its
	/// key's single shared outcome.
	pub async fn pull_services_with_options(
		&self,
		file: &ComposeFile,
		services: &[String],
		opts: PullOptions,
	) -> Result<()> {
		// Reject unknown service names up front, matching `docker compose pull`
		// (and `logs`), rather than silently doing nothing.
		for name in services {
			if !file.services.contains_key(name) {
				return Err(ComposeError::ServiceNotFound(name.clone()));
			}
		}

		// `--include-deps` widens the explicit service list to its transitive
		// depends_on closure; an empty list already means "every service".
		let wanted: Option<HashSet<String>> = match (services.is_empty(), opts.include_deps) {
			(true, _) => None,
			(false, true) => Some(pull_dep_closure(file, services)),
			(false, false) => Some(services.iter().cloned().collect()),
		};

		// Every service this pull pass covers, in file order — kept so the
		// per-service reporting loop below stays deterministic — paired with
		// the key that determines its actual pull request: the image
		// reference, the *resolved* pull policy (the `--pull` override
		// applied ahead of the service's own `pull_policy:`, see
		// `resolved_pull_policy`), and the platform. Resolved once here so
		// the dedup step below and the final reporting loop agree on exactly
		// the same value instead of recomputing it (and re-warning on an
		// unrecognized policy) twice.
		type PullKey<'a> = (&'a str, &'static str, Option<&'a str>);
		let candidates: Vec<(&str, &Service, PullKey)> = file
			.services
			.iter()
			.filter(|(name, s)| {
				s.image.is_some()
					&& wanted
						.as_ref()
						.is_none_or(|set| set.contains(name.as_str()))
			})
			// A typo'd `pull_policy:` would otherwise be dropped silently here
			// and then the per-service `pull_image` would fail downstream
			// with the same error, N times. Resolve once and let the
			// offending value short-circuit the whole batch (#1369).
			//
			// Collect into `Result`: the previous `.expect` here assumed the
			// `up` path had already rejected an unknown value, but standalone
			// `pull` never reaches `pull_image` before this dedup, so the
			// invariant was false and the binary panicked on every typo
			// (#1450).
			.map(|(name, s)| {
				let image = s.image.as_deref().unwrap_or_default();
				let policy = self.resolved_pull_policy(name.as_str(), s)?;
				let key = (image, policy, s.platform.as_deref());
				Ok((name.as_str(), s, key))
			})
			.collect::<Result<Vec<_>>>()?;

		// Dedup by that key: 50 services agreeing on image, resolved policy
		// and platform must issue one pull, not 50. Two services naming the
		// same image with a different resolved policy or platform get their
		// own key, and so their own pull — one representative service per
		// unique key is enough to issue it.
		let mut representative: HashMap<PullKey, &Service> = HashMap::new();
		for (_, service, key) in &candidates {
			representative.entry(*key).or_insert(service);
		}

		// Pull each unique key once, bounded, and record its outcome — the
		// same present/error pair the per-service loop used to compute for
		// itself, now shared by every service that agrees on image, resolved
		// policy and platform.
		let futs = representative.into_iter().map(|(key, service)| async move {
			// The libpod pull stream reports failure as an in-band progress
			// line, so `pull_image` returns Ok even when the pull failed;
			// confirm the image actually landed in local storage. Keep the
			// real transport error (e.g. socket unreachable) so a failed pull
			// surfaces the underlying cause rather than a generic message.
			// The policy was already resolved while building `candidates`, so
			// reuse it here instead of letting `pull_image` resolve (and
			// potentially re-warn about) it a second time.
			let pull_err = self
				.pull_image_with_policy(service, key.1, self.quiet_pull)
				.await
				.err()
				.map(|e| e.to_string());
			let present = self.image_present(key.0).await;
			(key, present, pull_err)
		});
		let outcomes: HashMap<PullKey, (bool, Option<String>)> =
			bounded_join_all(futs, MAX_PULL_CONCURRENCY)
				.await
				.into_iter()
				.map(|(key, present, err)| (key, (present, err)))
				.collect();

		for (name, _service, key) in candidates {
			let image = key.0;
			let (present, pull_err) = outcomes.get(&key).cloned().unwrap_or((false, None));
			// Presence alone is not success. A stale copy of the image already in
			// local storage makes the probe pass while the pull actually failed,
			// so `pull` against an unreachable registry exited 0 and reported
			// nothing — the same way `up --pull always` did (#1076). The pull
			// having reported an error is decisive; the probe only covers the
			// case where it reported nothing and the image still is not there.
			if present && pull_err.is_none() {
				continue;
			}
			if opts.ignore_failures {
				match &pull_err {
					Some(e) => tracing::warn!("pull {name} ({image}) failed — ignored: {e}"),
					None => tracing::warn!("pull {name} ({image}) failed — ignored"),
				}
			} else {
				let detail = pull_err.map(|e| format!(": {e}")).unwrap_or_default();
				return Err(ComposeError::Build(format!(
					"failed to pull image {image} for service {name}{detail}"
				)));
			}
		}
		Ok(())
	}

	pub(in crate::engine) async fn pull_image(
		&self,
		service_name: &str,
		service: &Service,
	) -> Result<()> {
		let pull_policy = self.resolved_pull_policy(service_name, service)?;
		self.pull_image_with_policy(service, pull_policy, self.quiet_pull)
			.await
	}

	/// [`Self::pull_image`] with no user-facing progress, whatever `--quiet-pull`
	/// says.
	///
	/// For the `up` prefetch, which only warms the cache: `up_one_service`'s own
	/// pull is the authoritative one, and reporting from both is what printed
	/// `Pulling` twice per image on `up` while a standalone `pull` printed it
	/// once.
	pub(in crate::engine) async fn pull_image_quietly(
		&self,
		service_name: &str,
		service: &Service,
	) -> Result<()> {
		let pull_policy = self.resolved_pull_policy(service_name, service)?;
		self.pull_image_with_policy(service, pull_policy, true)
			.await
	}

	/// Resolve the effective libpod pull policy for `service`: the
	/// engine-wide `--pull` override ([`Engine::with_up_overrides`]) takes
	/// precedence over the service's own `pull_policy:`, and an unrecognized
	/// value is rejected via [`pull_policy_checked`]. Shared by
	/// [`Self::pull_image`] and by the standalone-pull fan-out's dedup key
	/// ([`Self::pull_services_with_options`]), so both agree on exactly the
	/// same resolved value — an override collapses the dedup (every service
	/// resolves to the same policy), while differing per-service policies
	/// (no override set) keep it split.
	///
	/// A service declaring `x-podman-autoupdate: registry` resolves to
	/// `newer` when no `--pull` override is in effect, the extension's
	/// whole point is to check the registry on every `up`. The CLI
	/// override wins, because that is what `--pull` is for.
	fn resolved_pull_policy(&self, service_name: &str, service: &Service) -> Result<&'static str> {
		let requested = self.pull_policy_override.as_deref().or_else(|| {
			if let Ok(Some(crate::compose::types::AutoUpdate::Registry)) =
				service.podman_autoupdate()
			{
				return Some("newer");
			}
			service.pull_policy.as_deref()
		});
		pull_policy_checked(requested, service_name)
	}

	/// Issue the actual pull request for `service` against an
	/// already-resolved `pull_policy` (see [`Self::resolved_pull_policy`]).
	/// Split out of [`Self::pull_image`] so the standalone-pull fan-out —
	/// which must resolve the policy anyway to compute its dedup key — can
	/// reuse that value instead of resolving (and potentially re-warning
	/// about an unrecognized one) a second time.
	async fn pull_image_with_policy(
		&self,
		service: &Service,
		pull_policy: &str,
		quiet: bool,
	) -> Result<()> {
		let image = match &service.image {
			Some(img) => img.clone(),
			None => return Ok(()),
		};

		// Through the progress layer, not a bare `eprintln!`. This was the one
		// user-facing line in the binary that bypassed `ui` entirely, so it
		// ignored `PROGRESS_ENABLED` and an embedder that asked podup to stay
		// silent got it anyway — and there was never a matching `Pulled`, so a
		// pull that finished looked exactly like one that hung.
		if quiet {
			debug!("pulling {image}");
		} else {
			crate::ui::progress::start("Image", &image, "Pulling");
		}

		let mut query = format!("reference={}&policy={}", urlencoded(&image), pull_policy);
		if let Some(platform) = &service.platform {
			query.push_str(&format!("&platform={}", urlencoded(platform)));
		}

		let path = format!("{API_PREFIX}/images/pull?{query}");
		let resp = self
			.client
			.post_empty_stream(&path)
			.await
			.map_err(ComposeError::Podman)?;
		let mut stream = crate::libpod::parse_json_lines::<ImagePullProgress>(resp.into_body());

		// libpod reports a failed pull as an in-band `error` line on a 200
		// response, not as an HTTP status, so the first one has to be kept and
		// returned. It used to be warned about and dropped, which made every
		// caller believe the pull had succeeded.
		//
		// A transport error mid-stream stays a warning: the same
		// finished-stream-looks-like-an-error ambiguity that #1104 is about, and
		// unlike the in-band line it is not libpod telling us the pull failed.
		let mut pull_err: Option<String> = None;
		while let Some(result) = stream.next().await {
			match result {
				Ok(progress) => {
					if !progress.stream.is_empty() {
						debug!("{}", progress.stream.trim_end());
					}
					if !progress.error.is_empty() {
						warn!("pull error: {}", progress.error);
						pull_err.get_or_insert_with(|| progress.error.clone());
					}
				}
				Err(e) => warn!("pull warning: {e}"),
			}
		}

		match pull_err {
			Some(e) => {
				// Close the row before returning: an unfinished `start` on the
				// failure path leaves the live board spinning on `Pulling` forever
				// even though the operation is over (#1347).
				if !quiet {
					crate::ui::progress_line("Image", &image, "Failed");
				}
				Err(ComposeError::Build(format!("pull {image} failed: {e}")))
			}
			None => {
				if !quiet {
					crate::ui::progress_line("Image", &image, "Pulled");
				}
				Ok(())
			}
		}
	}

	/// Whether an image reference is present in local storage. Used by the
	/// `pull` command to verify each pull actually landed (the streaming pull
	/// endpoint reports failures as in-band progress lines, not an HTTP error),
	/// and by the `up` image-prefetch stage to skip a redundant pull request
	/// for an image a `missing`-policy service already has cached.
	pub(in crate::engine) async fn image_present(&self, image: &str) -> bool {
		let path = format!("{API_PREFIX}/images/{}/json", urlencoded(image));
		self.client
			.get_json::<crate::libpod::types::image::ImageInspect>(&path)
			.await
			.is_ok()
	}

	/// The 64-hex ID the image reference resolves to in local storage right
	/// now, or `None` when no such image is present.
	///
	/// A name is a pointer and this is what it points at. `up` uses it to tell
	/// whether an existing container is still bound to the image its service
	/// resolves to, which the config hash cannot say: a rebuild, a pull or a
	/// `podman tag` moves the name and leaves the hash alone (#1620). A
	/// transport error is returned rather than folded into `None`, because the
	/// caller's fallback for "unknown" is to recreate, and a flaky socket must
	/// not silently turn every skip into a rebuild.
	pub(in crate::engine) async fn image_id(&self, image: &str) -> Result<Option<String>> {
		let path = format!("{API_PREFIX}/images/{}/json", urlencoded(image));
		match self
			.client
			.get_json::<crate::libpod::types::image::ImageInspect>(&path)
			.await
		{
			Ok(inspect) => Ok(Some(inspect.id)),
			Err(e) if e.is_status(404) => Ok(None),
			Err(e) => Err(ComposeError::Podman(e)),
		}
	}
}

/// The transitive `depends_on` closure of `services` (including the services
/// themselves), for `pull --include-deps`.
fn pull_dep_closure(file: &ComposeFile, services: &[String]) -> HashSet<String> {
	let mut set = HashSet::new();
	let mut stack: Vec<String> = services.to_vec();
	while let Some(name) = stack.pop() {
		if !set.insert(name.clone()) {
			continue;
		}
		if let Some(svc) = file.services.get(&name) {
			for dep in svc.depends_on.service_names() {
				if !set.contains(&dep) {
					stack.push(dep);
				}
			}
		}
	}
	set
}

/// Map a compose `pull_policy:` value to the libpod images/pull `policy`
/// parameter. `if_not_present` is the spec alias for `missing`; `build` falls
/// back to `missing` here (its build behavior is handled by the caller). Returns
/// `None` for an unrecognized value so the caller can warn and default.
pub(in crate::engine) fn libpod_pull_policy(policy: Option<&str>) -> Option<&'static str> {
	match policy {
		Some("always") => Some("always"),
		Some("newer") => Some("newer"),
		Some("never") => Some("never"),
		None | Some("missing") | Some("if_not_present") | Some("build") => Some("missing"),
		Some(_) => None,
	}
}

/// Map a compose `pull_policy:` value to the libpod images/pull `policy`
/// parameter, rejecting an unrecognized value with a structured
/// [`PodmanError::Field`] that names both the compose service and the
/// offending value (#1443).
///
/// `service_name` is the compose service the policy was read from (e.g.
/// `"web"`); pass the empty string when the value came from the engine-wide
/// `--pull` override (no service context). The rejected value lands in the
/// error so a typo'd `pull_policy: alaways` on a specific service is reported
/// as `service.<name>: pull_policy: unknown pull policy "alaways" (value:
/// alaways)` — the actionable bit is which service and which value, not the
/// abstract policy name.
pub(in crate::engine) fn pull_policy_checked(
	policy: Option<&str>,
	service_name: &str,
) -> crate::error::Result<&'static str> {
	if let Some(p) = libpod_pull_policy(policy) {
		return Ok(p);
	}
	let value = policy.unwrap_or_default();
	let message = format!(
		"unknown pull policy {value:?}: accepted values are always, missing, newer, never \
		 (case-insensitive); if_not_present and build are accepted as aliases for missing"
	);
	Err(crate::error::ComposeError::Podman(
		crate::libpod::validate::spec_field_error(service_name, "pull_policy", value, message),
	))
}

#[cfg(test)]
#[path = "pull_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pull_typo_tests.rs"]
mod typo_tests;
