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
/// service plus a `build:` service are excluded entirely: the image
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

	e.prefetch_images(&file, &enabled, &None).await.unwrap();

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

	e.prefetch_images(&file, &enabled, &target_set)
		.await
		.unwrap();

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

/// A typo'd `pull_policy:` must error loud at the prefetch stage instead of
/// being treated as `missing` (#1443). Both the dedup-side check (the
/// service is *included* in the prefetch set when the policy is anything
/// but `never`) and the per-image future would have happily read the bad
/// value as `missing` before the fix, leaving `up` to exit 0 with the
/// wrong image and no diagnostic.
#[tokio::test]
#[cfg(unix)]
async fn prefetch_rejects_an_unknown_pull_policy() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let file =
		crate::parse_str("services:\n  web:\n    image: nginx:1.27\n    pull_policy: alaways\n")
			.unwrap();
	let enabled: HashSet<String> = file.services.keys().cloned().collect();

	let err = e
		.prefetch_images(&file, &enabled, &None)
		.await
		.expect_err("an unknown pull_policy must be rejected, not silently treated as missing");
	let msg = err.to_string();
	assert!(msg.contains("alaways"), "got {msg}");
	assert!(
		matches!(
			err,
			crate::error::ComposeError::Podman(crate::libpod::PodmanError::Field {
				ref service,
				ref field,
				ref value,
				..
			}) if service == "web" && field == "pull_policy" && value == "alaways"
		),
		"unknown pull_policy must surface as a Field error naming the offending service and value, got {err:?}"
	);
}

/// An invalid `--pull` override (no service context) must also propagate
/// out of the prefetch stage, the same bug as the per-service typo, just
/// applied to every service at once. Before the fix the dedup phase
/// would treat every service's effective policy as `missing` and warm
/// every cache it could reach with the wrong intent (#1443).
#[tokio::test]
#[cfg(unix)]
async fn prefetch_rejects_an_invalid_pull_override() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let mut e = engine_with(fake.client(), "proj");
	e.pull_policy_override = Some("alaways".to_string());

	let file = crate::parse_str("services:\n  web:\n    image: nginx:1.27\n").unwrap();
	let enabled: HashSet<String> = file.services.keys().cloned().collect();

	let err = e
		.prefetch_images(&file, &enabled, &None)
		.await
		.expect_err("an invalid --pull override must be rejected");
	assert!(
		matches!(
			err,
			crate::error::ComposeError::Podman(crate::libpod::PodmanError::Field {
				ref field,
				ref value,
				..
			}) if field == "pull_policy" && value == "alaways"
		),
		"override must surface as a Field error naming the field and value, got {err:?}"
	);
}
