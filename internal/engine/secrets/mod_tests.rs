use super::*;
use crate::libpod::Client;
use std::path::PathBuf;

fn engine_with_base(base: &str) -> Engine {
	Engine::with_base_dir(
		Client::new("unused"),
		"proj".to_string(),
		PathBuf::from(base),
	)
}

/// The path a `file:` payload will be read from, for the single planned secret.
fn only_file_path(engine: &Engine, yaml: &str) -> PathBuf {
	let file = crate::compose::parse_str_raw(yaml).unwrap();
	let union = collect_payload_union("proj", &file, &engine.base_dir).unwrap();
	assert_eq!(union.len(), 1);
	match union.into_values().next().unwrap() {
		Payload::File(p) => p,
		Payload::Inline(_) => panic!("expected a file payload"),
	}
}

#[test]
fn secret_file_relative_path_is_anchored_to_base_dir() {
	// A relative `file:` resolves against the project dir, not the Podman
	// service's cwd, the same as a bind-mount source, which is what this was.
	let base = PathBuf::from("/srv/project");
	let yaml = "services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    file: secret.txt\n";
	let engine = engine_with_base(&base.to_string_lossy());
	assert_eq!(only_file_path(&engine, yaml), base.join("secret.txt"));
}

#[cfg(unix)]
#[test]
fn config_file_absolute_path_is_passed_through() {
	// Absolute paths are honored unchanged, exactly as `volumes:` does.
	let yaml = "services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    file: /etc/app/cfg.yaml\n";
	let engine = engine_with_base("/srv/project");
	assert_eq!(
		only_file_path(&engine, yaml),
		PathBuf::from("/etc/app/cfg.yaml")
	);
}

#[test]
fn inline_union_dedups_shared_secret_across_services() {
	// Two services in the same project both reference the same inline secret.
	// The up-front union must create it once (one scoped name), not once per
	// service, which is what previously raced delete-then-create.
	let yaml = "services:\n  a:\n    image: nginx\n    secrets: [tok]\n  b:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    content: shared\n";
	let file = crate::compose::parse_str_raw(yaml).unwrap();
	let union = collect_payload_union("proj", &file, Path::new("/base")).unwrap();
	assert_eq!(union.len(), 1);
	assert!(matches!(
		union.get("proj_secret_tok"),
		Some(Payload::Inline(b)) if b.expose_secret() == b"shared"
	));
}

#[test]
fn payload_union_collects_every_source_podup_creates_but_not_external() {
	// The union spans secrets and configs across sources (distinct scoped names)
	// and excludes only `external:`, which podup never creates and must never
	// remove on `down`.
	let yaml = "services:\n  web:\n    image: nginx\n    secrets: [tok, ext, onfile]\n    configs: [cfg]\nsecrets:\n  tok:\n    content: s\n  ext:\n    external: true\n  onfile:\n    file: ./f.txt\nconfigs:\n  cfg:\n    content: c\n";
	let file = crate::compose::parse_str_raw(yaml).unwrap();
	let union = collect_payload_union("proj", &file, Path::new("/base")).unwrap();
	let mut names: Vec<&String> = union.keys().collect();
	names.sort();
	assert_eq!(
		names,
		vec!["proj_config_cfg", "proj_secret_onfile", "proj_secret_tok"]
	);
}

#[test]
fn external_secret_is_never_in_the_payload_union() {
	// podup does not create an `external:` secret, so it must never appear in
	// the union that `up` creates and `down` removes.
	let yaml = "services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n";
	let file = crate::compose::parse_str_raw(yaml).unwrap();
	let union = collect_payload_union("proj", &file, Path::new("/base")).unwrap();
	assert!(union.is_empty());
}
