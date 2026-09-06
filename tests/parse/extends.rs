use podup::{parse_file, parse_str};
use std::io::Write;

fn make_chain_yaml(depth: usize) -> String {
	let mut yaml = "services:\n".to_string();
	// Reverse order so the deepest service comes first, forcing full recursion.
	for i in (1..=depth).rev() {
		yaml.push_str(&format!("  s{i}:\n    extends: s{}\n", i - 1));
	}
	yaml.push_str("  s0:\n    image: alpine\n");
	yaml
}

#[test]
fn extends_same_file() {
	let yaml = r#"
services:
  base:
    image: alpine
    environment:
      LOG: info
  app:
    extends: base
"#;
	let file = parse_str(yaml).unwrap();
	assert_eq!(file.services["app"].image.as_deref(), Some("alpine"));
}

#[test]
fn extends_merges_environment() {
	let yaml = r#"
services:
  base:
    image: alpine
    environment:
      LOG: info
      KEEP: yes
  app:
    extends:
      service: base
    environment:
      LOG: debug
      EXTRA: ok
"#;
	let file = parse_str(yaml).unwrap();
	let env = file.services["app"].environment.to_map();
	// Override wins for LOG, base value preserved for KEEP, override-only EXTRA present.
	assert_eq!(
		env.get("LOG").and_then(|v| v.clone()).as_deref(),
		Some("debug")
	);
	assert_eq!(
		env.get("KEEP").and_then(|v| v.clone()).as_deref(),
		Some("yes")
	);
	assert_eq!(
		env.get("EXTRA").and_then(|v| v.clone()).as_deref(),
		Some("ok")
	);
}

#[test]
fn extends_override_image_wins() {
	let yaml = r#"
services:
  base:
    image: alpine
  app:
    extends: base
    image: nginx:alpine
"#;
	let file = parse_str(yaml).unwrap();
	assert_eq!(file.services["app"].image.as_deref(), Some("nginx:alpine"));
}

#[test]
fn extends_chains_through_multiple_levels() {
	let yaml = r#"
services:
  grand:
    image: alpine
    environment:
      A: 1
  parent:
    extends: grand
    environment:
      B: 2
  child:
    extends: parent
    environment:
      C: 3
"#;
	let file = parse_str(yaml).unwrap();
	let env = file.services["child"].environment.to_map();
	assert!(env.contains_key("A"));
	assert!(env.contains_key("B"));
	assert!(env.contains_key("C"));
	assert_eq!(file.services["child"].image.as_deref(), Some("alpine"));
}

#[test]
fn extends_circular_errors() {
	let yaml = r#"
services:
  a:
    image: alpine
    extends: b
  b:
    image: alpine
    extends: a
"#;
	assert!(parse_str(yaml).is_err());
}

#[test]
fn extends_self_errors() {
	let yaml = r#"
services:
  a:
    image: alpine
    extends: a
"#;
	assert!(parse_str(yaml).is_err());
}

#[test]
fn extends_unknown_service_errors() {
	let yaml = r#"
services:
  a:
    image: alpine
    extends: missing
"#;
	assert!(parse_str(yaml).is_err());
}

#[test]
fn extends_with_external_file() {
	let dir = tempfile::tempdir().unwrap();
	let common_path = dir.path().join("common.yml");
	let mut f = std::fs::File::create(&common_path).unwrap();
	writeln!(
		f,
		r#"
services:
  base:
    image: alpine
    environment:
      FROM_BASE: yes
"#
	)
	.unwrap();

	let main_path = dir.path().join("docker-compose.yml");
	let mut m = std::fs::File::create(&main_path).unwrap();
	writeln!(
		m,
		r#"
services:
  app:
    extends:
      service: base
      file: ./common.yml
    environment:
      FROM_APP: yes
"#
	)
	.unwrap();

	let file = parse_file(&main_path).unwrap();
	assert_eq!(file.services["app"].image.as_deref(), Some("alpine"));
	let env = file.services["app"].environment.to_map();
	assert!(env.contains_key("FROM_BASE"));
	assert!(env.contains_key("FROM_APP"));
}

#[test]
fn extends_external_file_parent_traversal_is_allowed() {
	// The compose file is trusted input: `extends.file` with `../` must resolve,
	// matching docker-compose and podman-compose (monorepo shared-base pattern).
	let dir = tempfile::tempdir().unwrap();

	let common_path = dir.path().join("common.yml");
	let mut f = std::fs::File::create(&common_path).unwrap();
	writeln!(
		f,
		r#"
services:
  base:
    image: alpine
"#
	)
	.unwrap();

	let sub = dir.path().join("stack");
	std::fs::create_dir(&sub).unwrap();
	let main_path = sub.join("docker-compose.yml");
	let mut m = std::fs::File::create(&main_path).unwrap();
	writeln!(
		m,
		r#"
services:
  app:
    extends:
      service: base
      file: ../common.yml
"#
	)
	.unwrap();

	let file = parse_file(&main_path).unwrap();
	assert_eq!(file.services["app"].image.as_deref(), Some("alpine"));
}

#[test]
fn extends_external_file_missing_service_errors() {
	// The external file exists but does not define the referenced base service.
	let dir = tempfile::tempdir().unwrap();
	let common_path = dir.path().join("common.yml");
	let mut f = std::fs::File::create(&common_path).unwrap();
	writeln!(f, "services:\n  other:\n    image: alpine\n").unwrap();

	let main_path = dir.path().join("docker-compose.yml");
	let mut m = std::fs::File::create(&main_path).unwrap();
	writeln!(
		m,
		"services:\n  app:\n    extends:\n      service: base\n      file: ./common.yml\n"
	)
	.unwrap();

	let err = parse_file(&main_path).unwrap_err();
	assert!(err.to_string().contains("base"));
}

#[test]
fn extends_external_file_circular_across_files_errors() {
	// app -> base(in common.yml) -> app(back in the main file) forms a cycle that
	// spans the on-disk extends resolver; it must be detected, not loop forever.
	let dir = tempfile::tempdir().unwrap();
	let common_path = dir.path().join("common.yml");
	let mut f = std::fs::File::create(&common_path).unwrap();
	writeln!(
		f,
		"services:\n  base:\n    image: alpine\n    extends:\n      service: app\n      file: ./docker-compose.yml\n"
	)
	.unwrap();

	let main_path = dir.path().join("docker-compose.yml");
	let mut m = std::fs::File::create(&main_path).unwrap();
	writeln!(
		m,
		"services:\n  app:\n    extends:\n      service: base\n      file: ./common.yml\n"
	)
	.unwrap();

	assert!(parse_file(&main_path).is_err());
}

#[test]
fn extends_external_chain_exceeds_max_depth_errors() {
	// A long cross-file extends chain trips the depth guard rather than recursing
	// without bound. Each file extends the next; 64 hops is well past the limit.
	let dir = tempfile::tempdir().unwrap();
	let levels = 64;
	for i in 0..levels {
		let path = dir.path().join(format!("svc{i}.yml"));
		let mut f = std::fs::File::create(&path).unwrap();
		writeln!(
			f,
			"services:\n  base:\n    image: alpine\n    extends:\n      service: base\n      file: ./svc{}.yml\n",
			i + 1
		)
		.unwrap();
	}

	let main_path = dir.path().join("docker-compose.yml");
	let mut m = std::fs::File::create(&main_path).unwrap();
	writeln!(
		m,
		"services:\n  app:\n    extends:\n      service: base\n      file: ./svc0.yml\n"
	)
	.unwrap();

	assert!(parse_file(&main_path).is_err());
}

#[test]
fn extends_external_file_anchors_relative_paths() {
	let dir = tempfile::tempdir().unwrap();
	let sub = dir.path().join("svc");
	std::fs::create_dir(&sub).unwrap();

	let common_path = sub.join("common.yml");
	let mut f = std::fs::File::create(&common_path).unwrap();
	writeln!(
		f,
		r#"
services:
  base:
    image: alpine
    env_file:
      - ./base.env
    volumes:
      - ./data:/data
"#
	)
	.unwrap();

	let main_path = dir.path().join("docker-compose.yml");
	let mut m = std::fs::File::create(&main_path).unwrap();
	writeln!(
		m,
		r#"
services:
  app:
    extends:
      service: base
      file: ./svc/common.yml
"#
	)
	.unwrap();

	let file = parse_file(&main_path).unwrap();
	let app = &file.services["app"];

	// The base service's relative env_file/volume paths must be anchored to the
	// external file's directory, not the top-level project directory.
	let entries = app.env_file.to_entries();
	let env_path = std::path::Path::new(entries[0].path());
	assert!(
		env_path.is_absolute(),
		"env_file not anchored: {env_path:?}"
	);
	assert!(
		env_path.ends_with("svc/base.env"),
		"wrong dir: {env_path:?}"
	);

	let vol = match &app.volumes[0] {
		podup::compose::types::VolumeMount::Short(s) => s.clone(),
		other => panic!("expected short volume, got {other:?}"),
	};
	assert!(
		vol.contains("svc") && vol.ends_with(":/data"),
		"volume bind not anchored: {vol}"
	);
}

#[test]
fn extends_chain_within_depth_limit() {
	// 16 services = 15 hops, which must succeed.
	let yaml = make_chain_yaml(15);
	assert!(
		parse_str(&yaml).is_ok(),
		"chain of 15 hops must be accepted"
	);
}

#[test]
fn extends_chain_exceeds_depth_limit() {
	// 17 services = 16 hops, which must be rejected.
	let yaml = make_chain_yaml(16);
	let err = parse_str(&yaml).unwrap_err();
	let msg = err.to_string();
	assert!(
		msg.contains("exceeds maximum depth"),
		"error must mention depth limit, got: {msg}"
	);
}

/// Write `yaml` to a `docker-compose.yml` in a fresh tempdir and parse it from
/// disk, returning `(dir, parse_result)`. The on-disk path exercises
/// `resolve_all_extends`/`resolve_one_extends` rather than the in-memory parser.
fn parse_file_yaml(
	yaml: &str,
) -> (
	tempfile::TempDir,
	podup::Result<podup::compose::types::ComposeFile>,
) {
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("docker-compose.yml");
	std::fs::write(&path, yaml).unwrap();
	let res = parse_file(&path);
	(dir, res)
}

#[test]
fn extends_same_file_chain_from_disk() {
	// A same-file `extends:` (no `file:`) parsed FROM DISK takes the on-disk
	// resolver's in-file recursion + merge path, not just the in-memory one.
	let (_dir, res) = parse_file_yaml(
		"services:\n  base:\n    image: alpine\n    environment:\n      ROOT: \"1\"\n  \
		 mid:\n    extends: base\n    environment:\n      MID: \"1\"\n  \
		 app:\n    extends: mid\n    environment:\n      APP: \"1\"\n",
	);
	let file = res.unwrap();
	let env = file.services["app"].environment.to_map();
	// The whole chain merged: app sees its own, mid's, and base's variables.
	assert_eq!(
		env.get("ROOT").and_then(|v| v.clone()).as_deref(),
		Some("1")
	);
	assert_eq!(env.get("MID").and_then(|v| v.clone()).as_deref(), Some("1"));
	assert_eq!(env.get("APP").and_then(|v| v.clone()).as_deref(), Some("1"));
}

#[test]
fn extends_same_file_self_reference_from_disk_errors() {
	let (_dir, res) = parse_file_yaml("services:\n  a:\n    image: alpine\n    extends: a\n");
	let err = res.unwrap_err();
	assert!(err.to_string().contains("extends itself"), "got: {err}");
}

#[test]
fn extends_same_file_unknown_target_from_disk_errors() {
	let (_dir, res) = parse_file_yaml("services:\n  a:\n    image: alpine\n    extends: ghost\n");
	let err = res.unwrap_err();
	assert!(
		err.to_string().contains("unknown service 'ghost'"),
		"got: {err}"
	);
}
