#[cfg(unix)]
use crate::engine::fake_podman;
#[cfg(unix)]
use crate::engine::secrets::tests_support::{engine_on, file_with_content_secrets};

/// A `/secrets/json` body holding one entry per `(name, project-label)` pair.
#[cfg(unix)]
fn secret_list(entries: &[(&str, &str)]) -> String {
	let items: Vec<String> = entries
		.iter()
		.map(|(name, project)| {
			format!(r#"{{"Spec":{{"Name":"{name}","Labels":{{"podup.project":"{project}"}}}}}}"#)
		})
		.collect();
	format!("[{}]", items.join(","))
}

/// #1263: the labelled list already answers the ownership question for every
/// name at once, so teardown must not also inspect each secret individually
/// for the same label. Measured on the six-secret bench scenario, dropping
/// those takes `down -v` from 18 requests to 12.
#[tokio::test]
#[cfg(unix)]
async fn down_uses_the_list_and_inspects_no_secret_individually() {
	let body = secret_list(&[("proj_secret_s1", "proj"), ("proj_secret_s2", "proj")]);
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/secrets/json") {
			(200, body.clone())
		} else {
			(200, "{}".to_string())
		}
	});
	let e = engine_on(&fake);

	e.remove_internal_secrets(&file_with_content_secrets(2))
		.await
		.expect("teardown should succeed");

	let seen = fake.requests.lock().unwrap().clone();
	let inspects: Vec<&String> = seen
		.iter()
		.filter(|r| r.starts_with("GET") && r.contains("/json") && !r.contains("/secrets/json"))
		.collect();
	assert!(
		inspects.is_empty(),
		"no per-secret inspect should be issued, got {inspects:?}"
	);
	assert_eq!(
		seen.iter().filter(|r| r.starts_with("DELETE")).count(),
		2,
		"both listed secrets are removed, got {seen:?}"
	);
}

/// The guard the batch has to keep: a secret carrying another project's label
/// is not in the owned set, so it is neither inspected nor removed, even
/// though the compose file names it.
#[tokio::test]
#[cfg(unix)]
async fn down_never_deletes_a_secret_labelled_for_another_project() {
	let body = secret_list(&[
		("proj_secret_s1", "proj"),
		("proj_secret_s2", "someone-else"),
	]);
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/secrets/json") {
			(200, body.clone())
		} else {
			(200, "{}".to_string())
		}
	});
	let e = engine_on(&fake);

	e.remove_internal_secrets(&file_with_content_secrets(2))
		.await
		.expect("teardown should succeed");

	let seen = fake.requests.lock().unwrap().clone();
	assert!(
		seen.iter()
			.any(|r| r.starts_with("DELETE") && r.contains("proj_secret_s1")),
		"our own secret is removed, got {seen:?}"
	);
	assert!(
		!seen.iter().any(|r| r.contains("proj_secret_s2")),
		"a secret labelled for another project must not be touched at all, got {seen:?}"
	);
}

/// A secret podup created whose compose key was since renamed or removed is
/// still swept, because the labelled list, not the compose file, is what
/// teardown walks.
#[tokio::test]
#[cfg(unix)]
async fn down_sweeps_an_orphan_the_compose_file_no_longer_names() {
	let body = secret_list(&[("proj_secret_gone", "proj")]);
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/secrets/json") {
			(200, body.clone())
		} else {
			(200, "{}".to_string())
		}
	});
	let e = engine_on(&fake);

	e.remove_internal_secrets(&file_with_content_secrets(1))
		.await
		.expect("teardown should succeed");

	let seen = fake.requests.lock().unwrap().clone();
	assert!(
		seen.iter()
			.any(|r| r.starts_with("DELETE") && r.contains("proj_secret_gone")),
		"an orphan carrying our label is still removed, got {seen:?}"
	);
}

/// The failure mode worth more than the saving. Since the list *is* the
/// ownership check now, a failed list must not read as "nothing is ours":
/// that would delete nothing and report a clean `down`. It falls back to the
/// per-secret guarded path instead.
#[tokio::test]
#[cfg(unix)]
async fn a_failed_list_falls_back_to_per_secret_inspection_not_to_deleting_nothing() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/secrets/json") {
			(500, r#"{"message":"boom"}"#.to_string())
		} else if method == "GET" {
			(
				200,
				r#"{"Spec":{"Labels":{"podup.project":"proj"}}}"#.to_string(),
			)
		} else {
			(200, "{}".to_string())
		}
	});
	let e = engine_on(&fake);

	e.remove_internal_secrets(&file_with_content_secrets(2))
		.await
		.expect("teardown should still succeed");

	let seen = fake.requests.lock().unwrap().clone();
	assert_eq!(
		seen.iter().filter(|r| r.starts_with("DELETE")).count(),
		2,
		"both compose-named secrets are still removed when the list fails, got {seen:?}"
	);
	assert!(
		seen.iter()
			.any(|r| r.starts_with("GET") && r.contains("proj_secret_s1/json")),
		"the fallback re-checks ownership per secret rather than assuming it, got {seen:?}"
	);
}
