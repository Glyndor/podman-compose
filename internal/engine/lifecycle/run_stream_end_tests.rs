use crate::engine::fake_podman::{self, FakeReply};
use crate::engine::Engine;

fn compose() -> crate::compose::types::ComposeFile {
	crate::parse_str("services:\n  app:\n    image: img\n").unwrap()
}

/// A fake whose log stream is cut with no terminating chunk, whose container
/// listing reports `state`, and whose `wait` reports `code`.
fn fake(state: &'static str, code: i64) -> fake_podman::FakePodman {
	fake_podman::start_replying(move |method, target| {
		if target.contains("/logs") {
			FakeReply::ChunkedTruncated(vec!["output\n".to_string()])
		} else if target.contains("/wait") {
			FakeReply::Body(200, code.to_string())
		} else if target.contains("/containers/json") {
			FakeReply::Body(
				200,
				format!(r#"[{{"Names":["/proj-app-run-1"],"State":"{state}"}}]"#),
			)
		} else if method == "POST" {
			FakeReply::Body(200, r#"{"Id":"cafe"}"#.to_string())
		} else {
			FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
		}
	})
}

fn options() -> crate::engine::RunOptions {
	crate::engine::RunOptions {
		cmd: vec![],
		rm: false,
		detach: false,
		env_overrides: vec![],
		name_override: Some("proj-app-run-1".to_string()),
		service_ports: false,
	}
}

#[tokio::test]
async fn a_cut_stream_after_the_container_stopped_reports_the_real_exit_code() {
	let fake = fake("exited", 0);
	let e = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());

	e.run(&compose(), "app", options())
		.await
		.expect("the command finished; only the terminator went missing");
}

#[tokio::test]
async fn a_cut_stream_while_the_container_runs_is_a_failure() {
	let fake = fake("running", 0);
	let e = Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir());

	let err = e
		.run(&compose(), "app", options())
		.await
		.expect_err("output was truncated with the container still up: that is a failed read");
	assert!(
		matches!(err, crate::error::ComposeError::Podman(_)),
		"expected the transport error to survive, got {err:?}"
	);
}
