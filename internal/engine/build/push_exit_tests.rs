//! `--ignore-push-failures` must not turn "every push failed" into a success.
//!
//! The flag means "do not stop at the first failure". It used to also mean
//! "never report failure": each error was logged as a warning, `try_push`
//! returned `Ok(())`, and the loop finished `Ok(())` however many failed. A
//! gate written as `podup push --ignore-push-failures && deploy.sh` therefore
//! deployed against a registry that had received nothing at all.
//!
//! The two cases below are the whole distinction. One survivor is the flag
//! working; no survivors is a failed push wearing a zero.

use super::PushOptions;
// Gated the way pull_tests.rs gates it: fake_podman binds a unix socket, so the
// import itself does not resolve on Windows. The #[cfg(unix)] on each test is
// not enough -- a module-level use is compiled everywhere.
#[cfg(unix)]
use crate::engine::fake_podman;

fn opts() -> PushOptions {
	PushOptions {
		ignore_failures: true,
		tls_verify: None,
	}
}

/// Every image fails. The flag must not hide that from the exit code.
#[tokio::test]
#[cfg(unix)]
async fn every_push_failing_is_an_error_even_when_failures_are_ignored() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/push") {
			(500, r#"{"message":"registry unreachable"}"#.to_string())
		} else {
			(200, "{}".to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file = crate::parse_str("services:\n  a:\n    image: one\n  b:\n    image: two\n").unwrap();

	let err = e
		.push_with_quiet(&file, &[], opts(), false)
		.await
		.expect_err("all pushes failed, so the run must not report success");

	let msg = format!("{err}");
	assert!(
		msg.contains("every push failed"),
		"the error must say every push failed, not name one image: {msg}"
	);
}

/// One image fails, one succeeds. That is what the flag is for, and it stays
/// a success -- otherwise the fix would have replaced one wrong answer with
/// another.
#[tokio::test]
#[cfg(unix)]
async fn one_survivor_keeps_the_run_successful() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/push") && target.contains("bad") {
			(500, r#"{"message":"registry unreachable"}"#.to_string())
		} else {
			(200, "{}".to_string())
		}
	});
	let e = crate::engine::Engine::new(fake.client(), "proj".into());
	let file =
		crate::parse_str("services:\n  a:\n    image: bad\n  b:\n    image: good\n").unwrap();

	e.push_with_quiet(&file, &[], opts(), false)
		.await
		.expect("one image failed and one succeeded: --ignore-push-failures must absorb that");
}
