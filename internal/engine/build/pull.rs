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
/// `#[non_exhaustive]` since 3.7.2, so a new flag can be added in a minor
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
	fn resolved_pull_policy(&self, service_name: &str, service: &Service) -> Result<&'static str> {
		let requested = self
			.pull_policy_override
			.as_deref()
			.or(service.pull_policy.as_deref());
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
mod tests {
	use super::{libpod_pull_policy, pull_dep_closure};

	#[test]
	fn dep_closure_includes_transitive_dependencies() {
		let file = crate::parse_str(
			"services:\n  web:\n    image: a\n    depends_on:\n      - api\n  api:\n    image: b\n    depends_on:\n      - db\n  db:\n    image: c\n  lone:\n    image: d\n",
		)
		.unwrap();
		let mut got: Vec<String> = pull_dep_closure(&file, &["web".to_string()])
			.into_iter()
			.collect();
		got.sort();
		assert_eq!(got, vec!["api", "db", "web"]);
	}

	#[test]
	fn dep_closure_of_leaf_is_just_itself() {
		let file = crate::parse_str("services:\n  db:\n    image: c\n").unwrap();
		let got: Vec<String> = pull_dep_closure(&file, &["db".to_string()])
			.into_iter()
			.collect();
		assert_eq!(got, vec!["db"]);
	}

	#[tokio::test]
	async fn pull_unknown_service_is_rejected() {
		// `pull bogus` must error on the unknown name instead of silently exiting 0.
		let file = crate::parse_str("services:\n  web:\n    image: a\n").unwrap();
		let e = crate::engine::Engine::new(
			crate::libpod::Client::new("/nonexistent.sock"),
			"proj".into(),
		);
		let err = e
			.pull_services(&file, &["nope".to_string()])
			.await
			.expect_err("unknown service must be rejected");
		assert!(
			matches!(err, crate::error::ComposeError::ServiceNotFound(_)),
			"unexpected error: {err:?}"
		);
	}

	// `pull_rejects_an_unknown_pull_policy_without_panicking` lives in the
	// sibling `pull_typo_tests.rs` to keep this file below the source-line
	// limit (#1450).

	#[test]
	fn pull_policy_maps_every_spec_value() {
		assert_eq!(libpod_pull_policy(Some("always")), Some("always"));
		assert_eq!(libpod_pull_policy(Some("newer")), Some("newer"));
		assert_eq!(libpod_pull_policy(Some("never")), Some("never"));
		assert_eq!(libpod_pull_policy(Some("missing")), Some("missing"));
		// `if_not_present` is the spec alias for `missing`.
		assert_eq!(libpod_pull_policy(Some("if_not_present")), Some("missing"));
		assert_eq!(libpod_pull_policy(Some("build")), Some("missing"));
		assert_eq!(libpod_pull_policy(None), Some("missing"));
		// Unknown values are reported (None) so the caller fails loud (#1369).
		assert_eq!(libpod_pull_policy(Some("bogus")), None);
	}

	/// A typo'd `pull_policy:` must surface as a hard error (#1369): the
	/// previous warn-and-default-to-missing path silently turned `alaways`
	/// into the opposite of what the user wrote, and a `never` typo pulled
	/// fresh images on every `up` without telling the operator.
	#[test]
	fn resolved_pull_policy_rejects_an_unknown_value() {
		let e = crate::engine::Engine::new(
			crate::libpod::Client::new("/nonexistent.sock"),
			"proj".into(),
		);
		let svc = crate::compose::types::Service {
			image: Some("nginx:1.27".to_string()),
			pull_policy: Some("alaways".to_string()),
			..crate::compose::types::Service::default()
		};
		let err = e.resolved_pull_policy("web", &svc).unwrap_err();
		let msg = err.to_string();
		assert!(msg.contains("alaways"), "got {msg}");
		assert!(msg.contains("always"), "got {msg}");
		assert!(
			matches!(
				err,
				crate::error::ComposeError::Podman(crate::libpod::PodmanError::Field {
					ref service,
					ref field,
					..
				}) if service == "web" && field == "pull_policy"
			),
			"unknown pull policy must surface as a Field error carrying the service and field name, got {err:?}"
		);
	}

	#[cfg(unix)]
	use crate::engine::fake_podman;

	/// #8: `pull_services_with_options` used to build one future per service,
	/// so two services sharing an image pulled it twice. They must now
	/// dedupe down to a single pull request, with both services still
	/// reported as successful.
	#[tokio::test]
	#[cfg(unix)]
	async fn pull_dedupes_a_shared_image_into_a_single_pull() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				(200, String::new())
			} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
				(200, "{}".to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = crate::engine::Engine::new(fake.client(), "proj".into());
		let file =
			crate::parse_str("services:\n  a:\n    image: shared\n  b:\n    image: shared\n")
				.unwrap();

		e.pull_services(&file, &[])
			.await
			.expect("pulling two services that share an image must succeed");

		let seen = fake.requests.lock().unwrap();
		let pulls = seen
			.iter()
			.filter(|r| r.contains("/images/pull") && r.contains("reference=shared"))
			.count();
		assert_eq!(
			pulls, 1,
			"two services sharing one image must issue a single pull: {seen:?}"
		);
	}

	/// Two services sharing an image but with *different* resolved pull
	/// policies (no `--pull` override) must each get their own pull request,
	/// not collapse into one — the dedup key must include the resolved
	/// policy, not just the image reference.
	#[tokio::test]
	#[cfg(unix)]
	async fn pull_issues_separate_requests_for_same_image_different_policy() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				(200, String::new())
			} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
				(200, "{}".to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = crate::engine::Engine::new(fake.client(), "proj".into());
		let file = crate::parse_str(
			"services:\n  a:\n    image: shared\n    pull_policy: never\n  b:\n    image: shared\n    pull_policy: always\n",
		)
		.unwrap();

		e.pull_services(&file, &[])
			.await
			.expect("differing per-service pull_policy must not fail the pull");

		let seen = fake.requests.lock().unwrap();
		let pulls: Vec<&String> = seen
			.iter()
			.filter(|r| r.contains("/images/pull") && r.contains("reference=shared"))
			.collect();
		assert_eq!(
			pulls.len(),
			2,
			"same image with different resolved policies must issue two pulls, not one: {seen:?}"
		);
		assert!(
			pulls.iter().any(|r| r.contains("policy=never")),
			"missing the never-policy pull: {seen:?}"
		);
		assert!(
			pulls.iter().any(|r| r.contains("policy=always")),
			"missing the always-policy pull: {seen:?}"
		);
	}

	/// A shared image that fails to pull must still be reported for *every*
	/// service that names it — derived from the one shared outcome, not from
	/// a redundant pull per service. `ignore_failures` lets both warnings
	/// through instead of aborting on the first.
	#[tokio::test]
	#[cfg(unix)]
	async fn pull_failure_on_a_shared_image_is_still_only_pulled_once() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				(500, r#"{"message":"registry unreachable"}"#.to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = crate::engine::Engine::new(fake.client(), "proj".into());
		let file =
			crate::parse_str("services:\n  a:\n    image: shared\n  b:\n    image: shared\n")
				.unwrap();

		let opts = super::PullOptions {
			ignore_failures: true,
			include_deps: false,
		};
		e.pull_services_with_options(&file, &[], opts)
			.await
			.expect("ignore_failures must not error even though the shared pull failed");

		let seen = fake.requests.lock().unwrap();
		let pulls = seen
			.iter()
			.filter(|r| r.contains("/images/pull") && r.contains("reference=shared"))
			.count();
		assert_eq!(
			pulls, 1,
			"a failing shared image must still be pulled once, not once per service: {seen:?}"
		);
	}

	/// Without `ignore_failures`, a shared image that never lands must still
	/// abort the whole pull — the per-service error report is derived from
	/// the image's single shared outcome, so the failure is not silently
	/// dropped for services 2..N once service 1 already reported it.
	#[tokio::test]
	#[cfg(unix)]
	async fn pull_failure_on_a_shared_image_aborts_without_ignore_failures() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				(500, r#"{"message":"registry unreachable"}"#.to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = crate::engine::Engine::new(fake.client(), "proj".into());
		let file =
			crate::parse_str("services:\n  a:\n    image: shared\n  b:\n    image: shared\n")
				.unwrap();

		let err = e
			.pull_services(&file, &[])
			.await
			.expect_err("a shared image that fails to pull must abort the pull");
		assert!(
			matches!(err, crate::error::ComposeError::Build(ref msg) if msg.contains("shared")),
			"unexpected error: {err:?}"
		);
	}

	// Bounding the standalone pull's concurrency (`MAX_PULL_CONCURRENCY`) is
	// exercised structurally rather than by asserting a live in-flight count:
	// `bounded_join_all` runs every future through the same
	// `buffer_unordered(MAX_PULL_CONCURRENCY)` dispatcher the lifecycle
	// fan-out's `join_bounded` uses (see `parallel::tests::
	// join_bounded_preserves_input_order`), and a synchronous fake responder
	// cannot observe real concurrency without a multi-thread runtime and a
	// blocking rendezvous — exactly the flakiness the testing standard rules
	// out. The dedup tests above already pin the dispatch contract (every
	// unique image is attempted, exactly once).

	/// #1076: libpod reports a failed pull as an in-band `error` line on a 200
	/// response. That line used to be warned about and dropped, so every caller
	/// believed the pull had succeeded.
	///
	/// The image is present here — a stale copy from an earlier pull — which is
	/// exactly the case a presence probe cannot catch: it passes while the pull
	/// failed. `up --pull always` against an unreachable registry therefore
	/// started yesterday's image and exited 0.
	#[tokio::test]
	#[cfg(unix)]
	async fn a_failed_pull_is_reported_even_when_a_stale_image_is_present() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				// 200 with an in-band error, the way libpod reports it.
				(
					200,
					r#"{"error":"initializing source: pinging container registry: connection refused"}"#
						.to_string(),
				)
			} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
				// The stale image is in local storage.
				(200, "{}".to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = crate::engine::Engine::new(fake.client(), "proj".into());
		let file = crate::parse_str("services:\n  a:\n    image: stale:v1\n").unwrap();

		let err = e
			.pull_services(&file, &[])
			.await
			.expect_err("a pull that libpod reported as failed must not be reported as success");
		assert!(
			format!("{err}").contains("connection refused"),
			"the underlying cause must survive: {err}"
		);
	}

	/// The escape hatch still works: `--ignore-pull-failures` is deliberately
	/// exit-0, and that must not change just because the failure is now visible.
	#[tokio::test]
	#[cfg(unix)]
	async fn ignore_pull_failures_still_exits_zero_on_an_in_band_error() {
		let fake = fake_podman::start(|method, target| {
			if method == "POST" && target.contains("/images/pull") {
				(200, r#"{"error":"connection refused"}"#.to_string())
			} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
				(200, "{}".to_string())
			} else {
				(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let e = crate::engine::Engine::new(fake.client(), "proj".into());
		let file = crate::parse_str("services:\n  a:\n    image: stale:v1\n").unwrap();

		e.pull_services_with_options(
			&file,
			&[],
			crate::engine::PullOptions {
				ignore_failures: true,
				..Default::default()
			},
		)
		.await
		.expect("--ignore-pull-failures must stay exit 0");
	}
}

#[cfg(test)]
#[path = "pull_typo_tests.rs"]
mod typo_tests;
