//! Tests for the build path: secret resolution, build-arg and `shm_size`
//! validation, and the tar payload a file-backed secret ships.
//!
//! Split out of `build/mod.rs` so the production code stays under the
//! source-line limit.

use crate::compose::types::{BuildConfig, UlimitConfig};

use super::{render_build_ulimits, Engine};
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

/// `build.ulimits` was reported as having no libpod mapping. It has one:
/// measured on podman 5.7.0, a build with `ulimits=["nofile=1234:1234"]` saw
/// 1234 where the same build without it saw 524288.
#[test]
fn a_pair_renders_as_soft_colon_hard() {
	let build = build_with_ulimits(&[("nofile", UlimitConfig::Pair { soft: 1, hard: 2 })]);
	assert_eq!(render_build_ulimits(&build), vec!["nofile=1:2"]);
}

/// A single value is both limits.
#[test]
fn a_single_value_fills_both_limits() {
	let build = build_with_ulimits(&[("nproc", UlimitConfig::Single(64))]);
	assert_eq!(render_build_ulimits(&build), vec!["nproc=64:64"]);
}

/// The value reaches a query string, so an unrecognised name is a typo or an
/// injection attempt and is dropped rather than forwarded.
#[test]
fn an_unknown_resource_name_is_dropped() {
	let build = build_with_ulimits(&[
		("bogus,inject=1", UlimitConfig::Single(1)),
		("nofile", UlimitConfig::Single(2)),
	]);
	assert_eq!(render_build_ulimits(&build), vec!["nofile=2:2"]);
}

/// Podman rejects a soft limit above the hard one. The container path clamps
/// instead of failing, so the build path matches it.
#[test]
fn a_soft_limit_above_the_hard_one_is_clamped() {
	let build = build_with_ulimits(&[("nofile", UlimitConfig::Pair { soft: 99, hard: 5 })]);
	assert_eq!(render_build_ulimits(&build), vec!["nofile=5:5"]);
}

/// A short-form `build: .` has no options block at all.
#[test]
fn a_context_only_build_renders_nothing() {
	let build = BuildConfig::Context(".".into());
	assert!(render_build_ulimits(&build).is_empty());
}

fn build_with_ulimits(entries: &[(&str, UlimitConfig)]) -> BuildConfig {
	let yaml = entries
		.iter()
		.map(|(name, cfg)| match cfg {
			UlimitConfig::Single(v) => format!("  {name}: {v}\n"),
			UlimitConfig::Pair { soft, hard } => {
				format!("  {name}:\n    soft: {soft}\n    hard: {hard}\n")
			}
		})
		.collect::<String>();
	serde_yaml::from_str(&format!("context: .\nulimits:\n{yaml}")).expect("build config parses")
}
