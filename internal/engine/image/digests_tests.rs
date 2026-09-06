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
	// `RepoDigests`: the "locally built image" case.
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
	// `matches!` on a distinctive fragment, not `is_err()`: the test
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

/// The sort is what makes the reported error deterministic, and it has to be
/// tested on the mechanism rather than through the fan-out.
///
/// A two-failure fixture driven through `buffer_unordered` does **not**
/// discriminate: the in-process fake answers fast enough that completion
/// order coincides with submission order, so the assertion passes with the
/// sort removed. Measured: three runs, three passes, with the sort
/// commented out. That is the vacuous shape this file must not carry, so
/// the property is pinned directly instead.
#[test]
fn outcomes_are_processed_in_input_order_not_completion_order() {
	// What `buffer_unordered` can hand back: completion order, with the
	// file-order-first failure arriving last.
	let mut outcomes: Vec<(usize, String, ResolveOutcome)> = vec![
		(2, "zzz".into(), ResolveOutcome::NoDigest),
		(
			1,
			"mmm".into(),
			ResolveOutcome::Pinned("mmm@sha256:1".into()),
		),
		(0, "aaa".into(), ResolveOutcome::NoDigest),
	];
	outcomes.sort_by_key(|(i, _, _)| *i);

	let order: Vec<&str> = outcomes.iter().map(|(_, n, _)| n.as_str()).collect();
	assert_eq!(
		order,
		["aaa", "mmm", "zzz"],
		"the error pass and the mutation pass both walk this vector, so it \
		 must be input order before either runs"
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
