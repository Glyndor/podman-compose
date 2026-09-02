use crate::compose::types::ComposeFile;
use crate::engine::fake_podman;
use crate::engine::secrets::tests_support::engine_on;

const MARKER: &str = "podup-secret-payload-marker-must-never-be-logged";

fn file_with_marker() -> ComposeFile {
	crate::compose::parse_str(&format!(
		"services:\n  app:\n    image: alpine\n    secrets:\n      - s1\nsecrets:\n  s1: {{content: \"{MARKER}\"}}\n"
	))
	.expect("fixture compose file should parse")
}

/// The engine talks to a Podman that refuses every write. The error that
/// comes back is the one an integration test would print.
#[tokio::test]
async fn a_failing_secret_create_does_not_put_the_payload_in_the_error() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/secrets/create") {
			(
				500,
				r#"{"message":"secret store is unavailable"}"#.to_string(),
			)
		} else if method == "GET" && target.contains("/secrets/") {
			(404, r#"{"message":"no such secret"}"#.to_string())
		} else {
			(201, r#"{"ID":"abc"}"#.to_string())
		}
	});
	let e = engine_on(&fake);

	let err = e
		.create_project_secrets(&file_with_marker())
		.await
		.expect_err("a 500 from the secret store must surface as an error");

	// Both renderings, because a test prints `{err:?}` and a user sees
	// `{err}`, and only one of them being clean is not a guarantee.
	let display = format!("{err}");
	let debug = format!("{err:?}");
	assert!(
		!display.contains(MARKER),
		"the secret payload reached the error's Display: {display}"
	);
	assert!(
		!debug.contains(MARKER),
		"the secret payload reached the error's Debug: {debug}"
	);
}

/// The control that makes the assertion above mean something. A test that
/// searches for a marker proves nothing unless the marker was in play: if
/// the fixture stopped carrying it, both assertions above would pass over
/// a compose file with no secret in it at all.
#[test]
fn the_fixture_actually_carries_the_marker() {
	let file = file_with_marker();
	let rendered = format!("{file:?}");
	assert!(
		rendered.contains(MARKER),
		"the fixture no longer carries the marker, so the assertions that \
		 look for it are searching for something that was never there"
	);
}
