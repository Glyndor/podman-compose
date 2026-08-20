//! Tests for the build path: secret resolution, build-arg and `shm_size`
//! validation, and the tar payload a file-backed secret ships.
//!
//! Split out of `build/mod.rs` so the production code stays under the
//! source-line limit.

use super::Engine;
use crate::libpod::Client;

fn engine(base: std::path::PathBuf) -> Engine {
	Engine::with_base_dir(Client::new("/nonexistent.sock"), "p".into(), base)
}

fn build_of(file: &crate::compose::types::ComposeFile) -> &crate::compose::types::BuildConfig {
	file.services["app"].build.as_ref().unwrap()
}

#[test]
fn build_secret_from_file_shipped_in_tar() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("token.txt"), b"s3cr3t").unwrap();
	let yaml = "services:\n  app:\n    build:\n      context: .\n      secrets:\n        - tok\nsecrets:\n  tok:\n    file: token.txt\n";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine(dir.path().to_path_buf());
	let (files, specs) = e.resolve_build_secrets(build_of(&file), &file).unwrap();
	assert_eq!(
		specs,
		vec!["id=tok,src=.podup-build-secret-tok".to_string()]
	);
	assert_eq!(files.len(), 1);
	assert_eq!(files[0].0, ".podup-build-secret-tok");
	assert_eq!(files[0].1, b"s3cr3t");
}

#[test]
fn build_secret_content_inlined() {
	let yaml = "services:\n  app:\n    build:\n      context: .\n      secrets:\n        - c\nsecrets:\n  c:\n    content: inline-value\n";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine(std::env::temp_dir());
	let (files, _) = e.resolve_build_secrets(build_of(&file), &file).unwrap();
	assert_eq!(files[0].1, b"inline-value");
}

#[test]
fn build_secret_external_is_skipped() {
	let yaml = "services:\n  app:\n    build:\n      context: .\n      secrets:\n        - ext\nsecrets:\n  ext:\n    external: true\n";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine(std::env::temp_dir());
	let (files, specs) = e.resolve_build_secrets(build_of(&file), &file).unwrap();
	assert!(files.is_empty());
	assert!(specs.is_empty());
}

#[tokio::test]
async fn empty_build_arg_key_is_rejected() {
	// `--build-arg =value` is a user typo Podman would silently ignore; we
	// reject it before contacting the daemon.
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	let yaml = "services:\n  app:\n    build:\n      context: .\n";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine(dir.path().to_path_buf());
	let opts = super::BuildOptions {
		build_args: vec!["=orphan".to_string()],
		..Default::default()
	};
	let err = e
		.build_service("app", &file.services["app"], &file, &opts)
		.await
		.expect_err("empty build-arg key must be rejected");
	assert!(
		err.to_string().contains("build-arg"),
		"unexpected error: {err}"
	);
}

#[tokio::test]
async fn invalid_shm_size_is_rejected() {
	// A malformed `build.shm_size` must error rather than silently fall back to
	// the default shm size.
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
	let yaml = "services:\n  app:\n    build:\n      context: .\n      shm_size: \"64mb!\"\n";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine(dir.path().to_path_buf());
	let err = e
		.build_service(
			"app",
			&file.services["app"],
			&file,
			&super::BuildOptions::default(),
		)
		.await
		.expect_err("malformed shm_size must be rejected");
	assert!(
		err.to_string().contains("shm_size"),
		"unexpected error: {err}"
	);
}

#[test]
fn build_secret_undefined_errors() {
	let yaml =
		"services:\n  app:\n    build:\n      context: .\n      secrets:\n        - missing\n";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine(std::env::temp_dir());
	assert!(e.resolve_build_secrets(build_of(&file), &file).is_err());
}
