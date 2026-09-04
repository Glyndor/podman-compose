//! The board `build` opens, and the row verbs that arrive as buildah streams
//! the build.
//!
//! Each test wires a fake libpod that answers `/libpod/build` with a canned
//! stream and (where it matters) `/libpod/images/{tag}/tag` for the
//! `build.tags` aliases. The board and the row verbs are observed through the
//! `progress::capture` harness, which is what makes the assertions
//! deterministic without a real Podman.

#[cfg(unix)]
mod tests {
	use crate::engine::fake_podman::{self, FakeReply};
	use crate::engine::Engine;
	use crate::ui::progress::capture::Capture;
	use crate::ui::progress::Kind;

	/// A small project whose only service has a `build:` and an `image:` so
	/// the libpod tag call (`POST /libpod/images/.../tag`) has somewhere to
	/// land when the fake answers it.
	const FILE: &str = "\
services:
  app:
    image: localhost/ux-talker:1
    build:
      context: .
";

	/// A context directory with a real Dockerfile so `build_service`'s
	/// `fs::metadata(context_path)` check passes. Returns the directory and
	/// its path; the directory lives as long as the returned `TempDir` does.
	fn context() -> (tempfile::TempDir, std::path::PathBuf) {
		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().to_path_buf();
		std::fs::write(
			path.join("Dockerfile"),
			b"FROM docker.io/library/alpine:3.20\nRUN echo hi\nCMD [\"echo\",\"hi\"]\n",
		)
		.expect("write Dockerfile");
		(dir, path)
	}

	/// A fake Podman that walks `/build`,` writes `chunks` as one well-formed
	/// chunked body (each chunk's payload is the JSON line libpod would have
	/// sent at that point; the parser splits on `\n`), and answers the
	/// `POST /libpod/images/{tag}/tag` call that `apply_extra_tags` issues
	/// for an `image:` on a build-section service. Every other request gets
	/// a 404.
	fn engine_streaming(
		chunks: Vec<String>,
		context: std::path::PathBuf,
	) -> (fake_podman::FakePodman, Engine) {
		let fake = fake_podman::start_replying(move |method, target| {
			if method == "POST" && target.contains("/build?") {
				FakeReply::ChunkedEnd(chunks.clone())
			} else if method == "POST" && target.contains("/images/") && target.contains("/tag") {
				FakeReply::Body(200, String::new())
			} else {
				FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let engine = Engine::with_base_dir(fake.client(), "proj".into(), context);
		(fake, engine)
	}

	/// As [`engine_streaming`], but `/build` answers with a non-2xx body so
	/// the stream reader surfaces the error rather than treating the line as
	/// normal output. Used by the failure-path tests.
	fn engine_with_body(
		status: u16,
		body: String,
		context: std::path::PathBuf,
	) -> (fake_podman::FakePodman, Engine) {
		let fake = fake_podman::start_replying(move |method, target| {
			if method == "POST" && target.contains("/build?") {
				FakeReply::Body(status, body.clone())
			} else {
				FakeReply::Body(404, r#"{"message":"not found"}"#.to_string())
			}
		});
		let engine = Engine::with_base_dir(fake.client(), "proj".into(), context);
		(fake, engine)
	}

	#[tokio::test]
	async fn build_draws_a_row_per_image_and_counts_steps() {
		// A three-step Dockerfile's worth of stream: each STEP line is the
		// row transition the test asserts on. The trailing `-->` line is
		// what `parse_image_id_line` matches to recover the id on stdout;
		// the test does not assert on stdout here (that path is covered
		// end-to-end by the contract suite against a real Podman).
		let chunks = vec![
			"{\"stream\":\"STEP 1/3: FROM docker.io/library/alpine:3.20\\n\"}\n".to_string(),
			"{\"stream\":\"--> 3f3c8b769775\\n\"}\n".to_string(),
			"{\"stream\":\"STEP 2/3: RUN echo hi\\n\"}\n".to_string(),
			"{\"stream\":\"--> Using cache\\n\"}\n".to_string(),
			"{\"stream\":\"STEP 3/3: CMD [\\\"echo\\\",\\\"hi\\\"]\\n\"}\n".to_string(),
			"{\"stream\":\"--> 9f3c8b769775\\n\"}\n".to_string(),
			"{\"stream\":\"COMMIT localhost/ux-talker:1\\n\"}\n".to_string(),
			"{\"stream\":\"--> sha256:1111111111111111111111111111111111111111111111111111111111111111\\n\"}\n".to_string(),
			"{\"stream\":\"Successfully tagged localhost/ux-talker:1\\n\"}\n".to_string(),
		];
		let (_dir, ctx_path) = context();
		let (_fake, engine) = engine_streaming(chunks, ctx_path);
		let file = crate::parse_str(FILE).expect("the fixture parses");

		let capture = Capture::start();
		engine
			.build_all_with_options(&file, &[], &crate::engine::BuildOptions::default())
			.await
			.expect("a streaming build the fake accepts succeeds");

		let names = capture.names();
		assert_eq!(
			names,
			vec!["localhost/ux-talker:1"],
			"one row per image the pass builds: {names:?}"
		);
		assert!(
			capture.rows().iter().all(|(kind, _)| *kind == Kind::Image),
			"the build row is an image row, not a container: {:?}",
			capture.rows()
		);

		let verbs: Vec<String> = capture
			.verbs()
			.into_iter()
			.filter(|(_, name, _)| name == "localhost/ux-talker:1")
			.map(|(_, _, verb)| verb)
			.collect();
		assert_eq!(
			verbs,
			vec![
				"Building".to_string(),
				"Building 1/3".to_string(),
				"Building 2/3".to_string(),
				"Building 3/3".to_string(),
				"Built".to_string(),
			],
			"each STEP n/m advances the verb and the success verb closes the row: {verbs:?}"
		);
		assert!(
			capture.every_board_ended(),
			"the board closes on the way out, even on success"
		);
	}

	#[tokio::test]
	async fn a_failed_build_prints_its_stream_once_before_the_error() {
		// An in-band error on a 200 response is how libpod reports a build
		// that compiled nothing: the build itself returned 200 with one
		// `error` JSON line. The test asserts the error reaches the caller.
		// The failure replay path itself runs through the same plumbing
		// (the replay on a terminal, #1681); the per-line stderr mirror in
		// a pipe is the contract test's job.
		let body = "{\"stream\":\"STEP 1/3: FROM docker.io/library/alpine:3.20\\n\"}\n\
		             {\"stream\":\"--> 3f3c8b769775\\n\"}\n\
		             {\"stream\":\"STEP 2/3: RUN false\\n\"}\n\
		             {\"stream\":\"--> running step\\n\"}\n\
		             {\"error\":\"The command '/bin/sh -c false' returned a non-zero code: 1\\n\"}\n"
			.to_string();
		let (_dir, ctx_path) = context();
		let (_fake, engine) = engine_with_body(200, body, ctx_path);
		let file = crate::parse_str(FILE).expect("the fixture parses");

		let err = engine
			.build_all_with_options(&file, &[], &crate::engine::BuildOptions::default())
			.await
			.expect_err("a build whose last line is an `error` field must fail the pass");
		let msg = err.to_string();
		assert!(
			msg.contains("returned a non-zero code"),
			"the in-band error reaches the caller unchanged: {msg}"
		);
	}

	#[test]
	fn build_stream_progress_parses_a_three_step_dockerfile() {
		// Unit test on the parser: three STEP lines plus their `-->` markers,
		// in arrival order. Mirrors what `build_draws_a_row_per_image_and_counts_steps`
		// asserts at the row level, kept here so the parser is testable
		// without an end-to-end harness: the only way to assert what the
		// row actually said.
		let mut progress = super::super::BuildStreamProgress::new();
		let verbs: Vec<String> = [
			"STEP 1/3: FROM docker.io/library/alpine:3.20",
			"--> 3f3c8b769775",
			"STEP 2/3: RUN echo hi",
			"--> Using cache",
			"STEP 3/3: CMD [\"echo\", \"hi\"]",
		]
		.iter()
		.filter_map(|line| progress.observe(line))
		.collect();
		assert_eq!(
			verbs,
			vec![
				"Building 1/3".to_string(),
				"Building 2/3".to_string(),
				"Building 3/3".to_string(),
			],
			"each STEP advances the row verb by one: {verbs:?}"
		);
	}

	#[test]
	fn parse_image_id_line_only_matches_the_bare_full_id() {
		// Buildah's stream, measured on Podman 5.7: the full id is a line of
		// 64 hex digits on its own, after `Successfully tagged`. The layer
		// markers (`--> 3f3c8b769775`), cache hits (`--> Using cache <digest>`)
		// and the closing `Successfully built <short-id>` must NOT match: a
		// script reading stdout would take them for the image id.
		let id = "d1a7420d91864dc804e255408877a5234dfcfc9302e2526209d1f60ffd17f90b";
		assert_eq!(
			super::super::parse_image_id_line(id).as_deref(),
			Some(id),
			"the bare 64-hex line is the image id"
		);
		assert_eq!(
			super::super::parse_image_id_line(&format!("{id}\n")).as_deref(),
			Some(id),
			"with the stream's trailing newline still attached"
		);
		for other in [
			"--> d1a7420d9186",
			&format!("--> Using cache {id}"),
			"Successfully built d1a7420d9186",
			&format!("--> sha256:{id}"),
			&id[..63],
			"g1a7420d91864dc804e255408877a5234dfcfc9302e2526209d1f60ffd17f90b",
		] {
			assert!(
				super::super::parse_image_id_line(other).is_none(),
				"{other:?} is not the image id line"
			);
		}
	}
}

#[cfg(not(unix))]
mod tests {}
