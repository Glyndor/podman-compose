#[cfg(unix)]
use crate::engine::fake_podman;
#[cfg(unix)]
use crate::engine::secrets::tests_support::{engine_on, file_with_content_secrets};

/// #1219: on a first `up` every secret inspect is a 404, so there is nothing
/// to remove: the delete-then-create was spending a round trip per secret
/// deleting a secret that does not exist. Measured on the six-secret bench
/// scenario, this is what takes a cold `up` from 25 requests to 19.
#[tokio::test]
#[cfg(unix)]
async fn create_skips_the_delete_when_the_secret_does_not_exist() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/secrets/") {
			(404, r#"{"message":"no such secret"}"#.to_string())
		} else {
			(201, r#"{"ID":"abc"}"#.to_string())
		}
	});
	let e = engine_on(&fake);

	e.create_project_secrets(&file_with_content_secrets(3))
		.await
		.expect("creating fresh secrets should succeed");

	let seen = fake.requests.lock().unwrap().clone();
	let deletes: Vec<&String> = seen.iter().filter(|r| r.starts_with("DELETE")).collect();
	assert!(
		deletes.is_empty(),
		"no delete should be issued for a secret that does not exist, got {deletes:?}"
	);
	assert_eq!(
		seen.iter()
			.filter(|r| r.contains("/secrets/create"))
			.count(),
		3,
		"every secret is still created"
	);
}

/// The other half of the same rule: a secret that IS there still has to be
/// removed before the create, because `replace=true` is rejected on some
/// Podman 5.x builds. Skipping the delete unconditionally would break
/// re-`up` idempotence, which is the reason the delete exists at all.
#[tokio::test]
#[cfg(unix)]
async fn create_still_deletes_a_secret_that_already_exists() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/secrets/") {
			(
				200,
				r#"{"Spec":{"Labels":{"podup.project":"proj"}}}"#.to_string(),
			)
		} else {
			(201, r#"{"ID":"abc"}"#.to_string())
		}
	});
	let e = engine_on(&fake);

	e.create_project_secrets(&file_with_content_secrets(2))
		.await
		.expect("replacing our own secrets should succeed");

	let seen = fake.requests.lock().unwrap().clone();
	assert_eq!(
		seen.iter().filter(|r| r.starts_with("DELETE")).count(),
		2,
		"each existing secret is removed before being recreated, got {seen:?}"
	);
}

/// The behaviour change the fan-out carries, stated in #1219 and asserted
/// here rather than left to the reader: the pass no longer stops at the
/// first failure, so every secret is attempted, and the error that surfaces
/// is the first *by name*, not whichever chain happened to lose the race.
#[tokio::test]
#[cfg(unix)]
async fn fan_out_attempts_every_secret_and_reports_the_first_by_name() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/secrets/") {
			(404, r#"{"message":"no such secret"}"#.to_string())
		} else if method == "POST" && (target.contains("s2") || target.contains("s3")) {
			(500, r#"{"message":"boom"}"#.to_string())
		} else {
			(201, r#"{"ID":"abc"}"#.to_string())
		}
	});
	let e = engine_on(&fake);

	let err = e
		.create_project_secrets(&file_with_content_secrets(4))
		.await
		.expect_err("a failing secret must still fail the stage");

	let seen = fake.requests.lock().unwrap().clone();
	for i in 1..=4 {
		assert!(
			seen.iter().any(|r| r.contains(&format!("s{i}"))),
			"secret s{i} should have been attempted despite an earlier failure, got {seen:?}"
		);
	}
	assert!(
		err.to_string().contains("s2"),
		"the first failure by name should be the one reported, got: {err}"
	);
}

/// Skipping the delete opens a window between the inspect and the create.
/// If something claims the name in it, `up` fails rather than clobbering
/// what arrived, but Podman's own message for that is an opaque 500, so
/// the failure has to name the race or the operator cannot act on it.
#[tokio::test]
#[cfg(unix)]
async fn a_name_claimed_after_the_inspect_fails_with_a_legible_message() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/secrets/") {
			(404, r#"{"message":"no such secret"}"#.to_string())
		} else {
			(500, r#"{"message":"secret name in use"}"#.to_string())
		}
	});
	let e = engine_on(&fake);

	let err = e
		.create_project_secrets(&file_with_content_secrets(1))
		.await
		.expect_err("a name claimed in the window must fail")
		.to_string();

	assert!(
		err.contains("in between"),
		"the message must explain the race, got: {err}"
	);
	assert!(
		err.contains("proj_secret_s1"),
		"the message must name which secret, got: {err}"
	);
	// The engine's own error is kept as the cause, since it is useful, but it must
	// not be the whole of what the operator is handed, which is what the bare
	// `ComposeError::Podman` passthrough would have given them.
	assert!(
		!err.starts_with("podman API error"),
		"the raw engine error must not be the message itself, got: {err}"
	);
}

/// The ownership guard is untouched by any of the above: a secret of the
/// same name that podup did not create is still refused, never deleted.
#[tokio::test]
#[cfg(unix)]
async fn a_foreign_secret_is_still_refused_and_never_deleted() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/secrets/") {
			(
				200,
				r#"{"Spec":{"Labels":{"podup.project":"someone-else"}}}"#.to_string(),
			)
		} else {
			(201, r#"{"ID":"abc"}"#.to_string())
		}
	});
	let e = engine_on(&fake);

	let err = e
		.create_project_secrets(&file_with_content_secrets(1))
		.await
		.expect_err("a foreign secret must not be overwritten")
		.to_string();

	assert!(err.contains("refusing to overwrite"), "got: {err}");
	let seen = fake.requests.lock().unwrap().clone();
	assert!(
		!seen.iter().any(|r| r.starts_with("DELETE")),
		"a foreign secret must never be deleted, got {seen:?}"
	);
}
