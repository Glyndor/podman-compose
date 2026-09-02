use super::super::context::build_context_tar;
use super::*;
use futures_util::StreamExt;

/// The streamed body must be byte-identical to the buffered tar the
/// non-streaming path produced: same entries, same gzip, just delivered in
/// chunks. This pins the equivalence the whole refactor rests on.
#[tokio::test]
async fn streamed_body_matches_buffered_tar() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\n").unwrap();
	std::fs::write(dir.path().join("app.txt"), "hello world").unwrap();

	let (producer, body) = context_body(
		dir.path().to_path_buf(),
		ContextSource::Dockerfile("Dockerfile".to_string()),
		Vec::new(),
	);
	futures_util::pin_mut!(body);
	let mut streamed = Vec::new();
	while let Some(item) = body.next().await {
		let frame = item.expect("no stream error");
		if let Ok(data) = frame.into_data() {
			streamed.extend_from_slice(&data);
		}
	}
	producer.await.expect("join").expect("producer succeeds");

	let buffered = build_context_tar(dir.path(), "Dockerfile", &[]).unwrap();
	assert_eq!(
		streamed, buffered,
		"streamed tar must be byte-identical to the buffered tar"
	);
}

/// A missing context directory surfaces as the producer's error, and the body
/// still terminates (the writer is dropped) rather than hanging.
#[tokio::test]
async fn missing_context_errors_via_producer() {
	let (producer, body) = context_body(
		std::path::PathBuf::from("/nonexistent/podup/context"),
		ContextSource::Dockerfile("Dockerfile".to_string()),
		Vec::new(),
	);
	futures_util::pin_mut!(body);
	while body.next().await.is_some() {}
	let produced = producer.await.expect("join");
	assert!(produced.is_err(), "walking a missing context must error");
}
