use super::super::types::Service;
use super::*;
use std::path::Path;

fn svc(image: &str) -> Service {
	Service {
		image: Some(image.to_string()),
		..Default::default()
	}
}

#[test]
fn merge_adds_service_from_other() {
	let mut target = ComposeFile::default();
	let mut other = ComposeFile::default();
	other.services.insert("db".to_string(), svc("postgres:16"));
	merge_compose_file(&mut target, other);
	assert!(target.services.contains_key("db"));
}

#[test]
fn merge_parent_wins_on_service_conflict() {
	let mut target = ComposeFile::default();
	target.services.insert("web".to_string(), svc("nginx:1.25"));
	let mut other = ComposeFile::default();
	other.services.insert("web".to_string(), svc("nginx:1.24"));
	merge_compose_file(&mut target, other);
	assert_eq!(target.services["web"].image.as_deref(), Some("nginx:1.25"));
}

#[test]
fn merge_adds_volume_from_other() {
	let mut target = ComposeFile::default();
	let mut other = ComposeFile::default();
	other.volumes.insert("data".to_string(), None);
	merge_compose_file(&mut target, other);
	assert!(target.volumes.contains_key("data"));
}

#[test]
fn merge_parent_wins_on_volume_conflict() {
	let mut target = ComposeFile::default();
	target.volumes.insert("data".to_string(), None);
	let mut other = ComposeFile::default();
	other.volumes.insert("data".to_string(), None);
	merge_compose_file(&mut target, other);
	assert_eq!(target.volumes.len(), 1);
}

#[test]
fn merge_adds_network_from_other() {
	let mut target = ComposeFile::default();
	let mut other = ComposeFile::default();
	other.networks.insert("backend".to_string(), None);
	merge_compose_file(&mut target, other);
	assert!(target.networks.contains_key("backend"));
}

#[test]
fn merge_adds_and_parent_wins_on_secret_conflict() {
	use super::super::types::SecretConfig;
	let secret = |f: &str| SecretConfig {
		file: Some(f.to_string()),
		..Default::default()
	};
	let mut target = ComposeFile::default();
	target
		.secrets
		.insert("tok".to_string(), secret("parent.txt"));
	let mut other = ComposeFile::default();
	other.secrets.insert("tok".to_string(), secret("child.txt"));
	other.secrets.insert("extra".to_string(), secret("e.txt"));
	merge_compose_file(&mut target, other);
	// Parent wins on conflict; the included-only secret is added.
	assert_eq!(target.secrets["tok"].file.as_deref(), Some("parent.txt"));
	assert_eq!(target.secrets["extra"].file.as_deref(), Some("e.txt"));
}

#[test]
fn merge_adds_and_parent_wins_on_model_conflict() {
	use super::super::types::ModelConfig;
	let model = |m: &str| ModelConfig {
		model: Some(m.to_string()),
		..Default::default()
	};
	let mut target = ComposeFile::default();
	target.models.insert("llm".to_string(), model("parent/m"));
	let mut other = ComposeFile::default();
	other.models.insert("llm".to_string(), model("child/m"));
	other.models.insert("extra".to_string(), model("e/m"));
	merge_compose_file(&mut target, other);
	// Parent wins on conflict; the included-only model is added.
	assert_eq!(target.models["llm"].model.as_deref(), Some("parent/m"));
	assert_eq!(target.models["extra"].model.as_deref(), Some("e/m"));
}

#[test]
fn merge_adds_and_parent_wins_on_config_conflict() {
	use super::super::types::ConfigConfig;
	let config = |f: &str| ConfigConfig {
		file: Some(f.to_string()),
		..Default::default()
	};
	let mut target = ComposeFile::default();
	target
		.configs
		.insert("cfg".to_string(), config("parent.conf"));
	let mut other = ComposeFile::default();
	other
		.configs
		.insert("cfg".to_string(), config("child.conf"));
	other.configs.insert("only".to_string(), config("o.conf"));
	merge_compose_file(&mut target, other);
	assert_eq!(target.configs["cfg"].file.as_deref(), Some("parent.conf"));
	assert_eq!(target.configs["only"].file.as_deref(), Some("o.conf"));
}

#[test]
fn merge_empty_other_is_noop() {
	let mut target = ComposeFile::default();
	target.services.insert("web".to_string(), svc("nginx:1.25"));
	let other = ComposeFile::default();
	merge_compose_file(&mut target, other);
	assert_eq!(target.services.len(), 1);
}

// parse_included_file wraps every failure in ComposeError::Include (#1500).
//
// Each rejection test asserts the variant with `matches!` so the assertion
// cannot be satisfied by a different failure mode. Every rejection is paired
// with an acceptance test of the same shape, because a fixture that fails for
// the wrong reason still satisfies `expect_err` and proves nothing.

fn write_file(path: &Path, body: &str) {
	std::fs::write(path, body).expect("write fixture");
}

fn parse_main(main: &Path) -> crate::error::Result<ComposeFile> {
	crate::compose::parse_file(main)
}

#[test]
fn included_file_missing_becomes_include_variant() {
	// The `include:` points at a path that does not exist. Before the fix
	// this surfaced as `ComposeError::FileNotFound`; it must now surface as
	// `ComposeError::Include` so the consumer can tell a missing include from
	// a missing main file.
	let dir = tempfile::tempdir().expect("tempdir");
	let main = dir.path().join("docker-compose.yml");
	write_file(
		&main,
		"include:\n  - ./missing.yml\nservices:\n  app:\n    image: nginx\n",
	);
	let err = parse_main(&main).expect_err("missing include must error");
	assert!(
		matches!(err, ComposeError::Include(_)),
		"expected Include, got {err:?}"
	);
	// The path of the missing include is named in the message.
	assert!(
		err.to_string().contains("missing.yml"),
		"message should name the include path, got: {err}"
	);
	// Acceptance shape: the same compose succeeds when the include is present,
	// so a rejection above is the include path firing and not something else.
	let present = dir.path().join("present.yml");
	write_file(&present, "services:\n  helper:\n    image: alpine\n");
	let main_ok = dir.path().join("ok.yml");
	write_file(
		&main_ok,
		"include:\n  - ./present.yml\nservices:\n  app:\n    image: nginx\n",
	);
	let ok = parse_main(&main_ok).expect("present include must succeed");
	assert!(ok.services.contains_key("helper"));
}

#[test]
fn included_file_invalid_yaml_becomes_include_variant() {
	// The included file has valid YAML semantics *except* it contains a
	// type error. Before the fix this surfaced as `ComposeError::Parse`
	// with no hint that the failure was in the included file; it must now
	// surface as `ComposeError::Include`.
	let dir = tempfile::tempdir().expect("tempdir");
	let bad = dir.path().join("bad.yml");
	// `services` must be a mapping; a sequence is a type error.
	write_file(&bad, "services:\n  - not\n  - a\n  - mapping\n");
	let main = dir.path().join("docker-compose.yml");
	write_file(
		&main,
		"include:\n  - ./bad.yml\nservices:\n  app:\n    image: nginx\n",
	);
	let err = parse_main(&main).expect_err("malformed include must error");
	assert!(
		matches!(err, ComposeError::Include(_)),
		"expected Include, got {err:?}"
	);
	// The message names the included file so the operator can find it.
	assert!(
		err.to_string().contains("bad.yml"),
		"message should name the include path, got: {err}"
	);
	// Acceptance shape: the same main file with a well-formed include
	// succeeds, so the rejection above is the malformed-include path
	// firing, not a YAML error in the main file itself.
	let good = dir.path().join("good.yml");
	write_file(&good, "services:\n  helper:\n    image: alpine\n");
	let main_ok = dir.path().join("ok.yml");
	write_file(
		&main_ok,
		"include:\n  - ./good.yml\nservices:\n  app:\n    image: nginx\n",
	);
	assert!(parse_main(&main_ok).is_ok());
}

#[test]
fn valid_include_does_not_surface_include_variant() {
	// The acceptance shape for the rejection tests above. A well-formed
	// include must succeed and must NOT emit Include; if it did, the
	// rejection tests would pass for the wrong reason.
	let dir = tempfile::tempdir().expect("tempdir");
	let included = dir.path().join("included.yml");
	write_file(&included, "services:\n  helper:\n    image: alpine\n");
	let main = dir.path().join("docker-compose.yml");
	write_file(
		&main,
		"include:\n  - ./included.yml\nservices:\n  app:\n    image: nginx\n",
	);
	let file = parse_main(&main).expect("valid include must succeed");
	assert!(file.services.contains_key("app"));
	assert!(file.services.contains_key("helper"));
}

#[test]
fn included_path_is_directory_becomes_include_variant() {
	// Pointing `include:` at a directory provokes a non-NotFound io error
	// from the file reader (open-fails with IsADirectory on Unix, an
	// access error on Windows). The wrapping must convert it to
	// `Include`, not let `Io` leak out; that's the catch-all arm.
	let dir = tempfile::tempdir().expect("tempdir");
	let not_a_file = dir.path().join("a-directory");
	std::fs::create_dir(&not_a_file).expect("mkdir");
	let main = dir.path().join("docker-compose.yml");
	write_file(
		&main,
		"include:\n  - ./a-directory\nservices:\n  app:\n    image: nginx\n",
	);
	let err = parse_main(&main).expect_err("directory include must error");
	assert!(
		matches!(err, ComposeError::Include(_)),
		"expected Include, got {err:?}"
	);
}
