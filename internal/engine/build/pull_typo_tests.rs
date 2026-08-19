//! Targeted regression for #1450 — the standalone `pull` path used to panic
//! on a typo'd `pull_policy:` because the dedup pass unwrapped
//! `resolved_pull_policy` with an `.expect` whose invariant only held for
//! `up`. Split out so adding this test does not push `build/pull.rs` over the
//! source-line limit; the production fix lives there.

/// Standalone `pull` must surface a typo'd `pull_policy:` as a structured
/// `PodmanError::Field` carrying the offending service and value — the same
/// shape `up` uses after #1443. Before the fix the dedup pass unwrapped
/// `resolved_pull_policy` with `.expect`, so the binary panicked on every
/// typo and the worker-thread death produced a second `internal error` from
/// `main.rs:61`.
#[tokio::test]
async fn pull_rejects_an_unknown_pull_policy_without_panicking() {
	let file =
		crate::parse_str("services:\n  web:\n    image: alpine:latest\n    pull_policy: alaways\n")
			.unwrap();
	let e = crate::engine::Engine::new(
		crate::libpod::Client::new("/nonexistent.sock"),
		"proj".into(),
	);
	let err = e
		.pull_services(&file, &[])
		.await
		.expect_err("unknown pull_policy must surface as Err, not panic");
	let crate::error::ComposeError::Podman(crate::libpod::PodmanError::Field {
		ref service,
		ref field,
		ref value,
		..
	}) = err
	else {
		panic!("expected Field error, got {err:?}");
	};
	assert_eq!(service, "web");
	assert_eq!(field, "pull_policy");
	assert!(value.contains("alaways"), "got {value}");
}
