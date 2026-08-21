use podup::{parse_file, ComposeError};
use std::io::Write;

#[test]
fn include_string_form_merges_services() {
	let dir = tempfile::tempdir().unwrap();

	let included = dir.path().join("services.yml");
	writeln!(
		std::fs::File::create(&included).unwrap(),
		r#"
services:
  helper:
    image: alpine
"#
	)
	.unwrap();

	let main = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main).unwrap(),
		r#"
include:
  - ./services.yml

services:
  app:
    image: nginx
"#
	)
	.unwrap();

	let file = parse_file(&main).unwrap();
	assert!(file.services.contains_key("app"));
	assert!(file.services.contains_key("helper"));
}

#[test]
fn include_parent_relative_path_resolves() {
	// The Compose Specification treats `../` as a canonical include path
	// (monorepos reference shared compose files one level up). It must resolve,
	// not be rejected as path traversal.
	let dir = tempfile::tempdir().unwrap();

	let shared = dir.path().join("shared.yml");
	writeln!(
		std::fs::File::create(&shared).unwrap(),
		r#"
services:
  shared_svc:
    image: alpine
"#
	)
	.unwrap();

	let sub = dir.path().join("project");
	std::fs::create_dir(&sub).unwrap();
	let main = sub.join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main).unwrap(),
		r#"
include:
  - ../shared.yml

services:
  app:
    image: nginx
"#
	)
	.unwrap();

	let file = parse_file(&main).unwrap();
	assert!(file.services.contains_key("app"));
	assert!(file.services.contains_key("shared_svc"));
}

#[test]
fn include_absolute_path_resolves() {
	// docker-compose accepts absolute include paths; the compose file is trusted
	// input (like a Makefile), so podup resolves them as given rather than
	// rejecting them.
	let dir = tempfile::tempdir().unwrap();

	let shared = dir.path().join("shared.yml");
	writeln!(
		std::fs::File::create(&shared).unwrap(),
		r#"
services:
  shared_svc:
    image: alpine
"#
	)
	.unwrap();

	let main = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main).unwrap(),
		r#"
include:
  - {}

services:
  app:
    image: nginx
"#,
		shared.display()
	)
	.unwrap();

	let file = parse_file(&main).unwrap();
	assert!(file.services.contains_key("app"));
	assert!(file.services.contains_key("shared_svc"));
}

#[test]
fn include_long_form_parses() {
	let dir = tempfile::tempdir().unwrap();

	let inc = dir.path().join("inc.yml");
	writeln!(
		std::fs::File::create(&inc).unwrap(),
		r#"
services:
  inc_svc:
    image: alpine
"#
	)
	.unwrap();

	let main = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main).unwrap(),
		r#"
include:
  - path: ./inc.yml

services:
  main_svc:
    image: alpine
"#
	)
	.unwrap();

	let file = parse_file(&main).unwrap();
	assert!(file.services.contains_key("inc_svc"));
	assert!(file.services.contains_key("main_svc"));
}

#[test]
fn parent_overrides_included_service() {
	let dir = tempfile::tempdir().unwrap();

	let inc = dir.path().join("inc.yml");
	writeln!(
		std::fs::File::create(&inc).unwrap(),
		r#"
services:
  shared:
    image: alpine:included
"#
	)
	.unwrap();

	let main = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main).unwrap(),
		r#"
include:
  - ./inc.yml

services:
  shared:
    image: alpine:override
"#
	)
	.unwrap();

	let file = parse_file(&main).unwrap();
	// Parent file definition wins.
	assert_eq!(
		file.services["shared"].image.as_deref(),
		Some("alpine:override")
	);
}

#[test]
fn global_env_file_feeds_interpolation() {
	let dir = tempfile::tempdir().unwrap();

	let env_path = dir.path().join("prod.env");
	let mut e = std::fs::File::create(&env_path).unwrap();
	writeln!(e, "IMG=nginx:1.27").unwrap();

	let main_path = dir.path().join("docker-compose.yml");
	let mut m = std::fs::File::create(&main_path).unwrap();
	writeln!(m, "services:\n  web:\n    image: ${{IMG}}").unwrap();

	let file = podup::parse_file_with_env_files(&main_path, &["prod.env".to_string()]).unwrap();
	assert_eq!(file.services["web"].image.as_deref(), Some("nginx:1.27"));
}

#[test]
fn multiple_files_merge_with_override() {
	let dir = tempfile::tempdir().unwrap();

	let base = dir.path().join("base.yml");
	let mut b = std::fs::File::create(&base).unwrap();
	writeln!(
		b,
		"services:\n  web:\n    image: nginx:1.0\n    environment:\n      A: \"1\"\n"
	)
	.unwrap();

	let over = dir.path().join("override.yml");
	let mut o = std::fs::File::create(&over).unwrap();
	writeln!(
		o,
		"services:\n  web:\n    image: nginx:2.0\n    environment:\n      B: \"2\"\n  db:\n    image: postgres:16\n"
	)
	.unwrap();

	let file = podup::parse_files_with_env_files(&[base, over], &[]).unwrap();

	// Later file overrides the image and adds a service; environment keys merge.
	assert_eq!(file.services["web"].image.as_deref(), Some("nginx:2.0"));
	assert!(file.services.contains_key("db"));
	let env = file.services["web"].environment.to_map();
	assert!(env.contains_key("A"));
	assert!(env.contains_key("B"));
}

// `include:` failure surfaces as `ComposeError::Include` (#1500).
//
// Before the fix the same failure surfaced as `ComposeError::FileNotFound`,
// `ComposeError::Parse`, or nothing distinguishable at all. Each rejection
// test asserts the variant with `matches!`, so the assertion cannot be
// satisfied by a different failure mode. Every rejection is paired with an
// acceptance test of the same shape, because a fixture that fails for the
// wrong reason still satisfies `expect_err` and proves nothing.

#[test]
fn include_missing_path_surfaces_as_include_variant() {
	let dir = tempfile::tempdir().unwrap();
	let main = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main).unwrap(),
		"include:\n  - ./not-here.yml\nservices:\n  app:\n    image: alpine\n"
	)
	.unwrap();
	let err = parse_file(&main).expect_err("missing include must error");
	assert!(
		matches!(err, ComposeError::Include(_)),
		"expected Include, got {err:?}"
	);
	// Acceptance shape: a present include succeeds, so the rejection above is
	// the missing-include path firing and not the parse path.
	let present = dir.path().join("present.yml");
	writeln!(
		std::fs::File::create(&present).unwrap(),
		"services:\n  helper:\n    image: alpine\n"
	)
	.unwrap();
	let main_ok = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main_ok).unwrap(),
		"include:\n  - ./present.yml\nservices:\n  app:\n    image: alpine\n"
	)
	.unwrap();
	let ok = parse_file(&main_ok).expect("present include must succeed");
	assert!(ok.services.contains_key("helper"));
}

#[test]
fn include_invalid_yaml_surfaces_as_include_variant() {
	let dir = tempfile::tempdir().unwrap();
	// `services` must be a mapping; a sequence is a YAML type error.
	let bad = dir.path().join("bad.yml");
	writeln!(
		std::fs::File::create(&bad).unwrap(),
		"services:\n  - not\n  - a\n  - mapping\n"
	)
	.unwrap();
	let main = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main).unwrap(),
		"include:\n  - ./bad.yml\nservices:\n  app:\n    image: alpine\n"
	)
	.unwrap();
	let err = parse_file(&main).expect_err("malformed include must error");
	assert!(
		matches!(err, ComposeError::Include(_)),
		"expected Include, got {err:?}"
	);
	// Acceptance shape: a well-formed include succeeds, so the rejection above
	// is the malformed-include path firing and not a YAML error in the main file.
	let good = dir.path().join("good.yml");
	writeln!(
		std::fs::File::create(&good).unwrap(),
		"services:\n  helper:\n    image: alpine\n"
	)
	.unwrap();
	let main_ok = dir.path().join("docker-compose.yml");
	writeln!(
		std::fs::File::create(&main_ok).unwrap(),
		"include:\n  - ./good.yml\nservices:\n  app:\n    image: alpine\n"
	)
	.unwrap();
	assert!(parse_file(&main_ok).is_ok());
}
