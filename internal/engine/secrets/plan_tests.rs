use super::*;

fn plans(yaml: &str) -> Vec<NativePlan> {
	let file = crate::compose::parse_str_raw(yaml).unwrap();
	collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).unwrap()
}

/// The inline bytes of a plan's payload, or `None` for an external/file source.
fn inline_bytes(p: &NativePlan) -> Option<&[u8]> {
	match &p.payload {
		Some(Payload::Inline(b)) => Some(b.expose_secret()),
		_ => None,
	}
}

#[test]
fn file_secret_is_a_scoped_native_secret_carrying_its_path() {
	// A `file:` secret is a project-scoped native secret like any other source.
	// The plan carries the resolved path, not the bytes: this module does no I/O,
	// so the read belongs to the layer that creates the secret.
	let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    file: ./tok.txt\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].source, "proj_secret_tok");
	assert_eq!(p[0].target, "tok");
	assert!(
		matches!(&p[0].payload, Some(Payload::File(path)) if path == Path::new("/base/tok.txt"))
	);
}

#[test]
fn file_secret_leaves_mode_unset_for_the_host_bits() {
	// With no `mode:` the plan stays unset so the effectful layer can mirror the
	// host file's own bits, rather than widening a 0600 secret to the 0444 the
	// other sources default to.
	let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    file: ./tok.txt\n");
	assert_eq!(p[0].mode, None);
}

#[test]
fn file_secret_honours_an_explicit_mode() {
	let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 0400\nsecrets:\n  tok:\n    file: ./tok.txt\n");
	assert_eq!(p[0].mode, Some(0o400));
}

#[test]
fn file_config_is_native_with_absolute_default_target() {
	let p = plans("services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    file: ./cfg.yaml\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].source, "proj_config_cfg");
	assert_eq!(p[0].target, "/cfg");
}

#[test]
fn file_secret_with_unsafe_name_is_rejected() {
	// The compose key becomes part of a Podman secret name for a `file:` source
	// too, so it faces the same bound as an inline one.
	let file = crate::compose::parse_str_raw(
		"services:\n  web:\n    image: x\n    secrets: ['../evil']\nsecrets:\n  '../evil':\n    file: ./tok.txt\n",
	)
	.unwrap();
	assert!(
		collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
	);
}

#[test]
fn empty_def_contributes_no_plan() {
	// A def with no content:, environment:, file: or external: is not a secret
	// podup can produce anything for.
	let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok: {}\n");
	assert!(p.is_empty());
}

// Unix-only: `PermissionsExt` does not exist on Windows, where a host file has
// no mode to mirror and `host_file_secret_mode` returns the 0444 default.
#[cfg(unix)]
#[test]
fn host_file_mode_masks_execute_and_special_bits() {
	// A secret holds data, never code. Mirroring a 0755 host file verbatim would
	// trip the dangerous-mode guard and fail an `up` that used to work.
	let dir = tempfile::tempdir().unwrap();
	let path = dir.path().join("s.txt");
	std::fs::write(&path, b"x").unwrap();
	for (host, want) in [
		(0o644, 0o644),
		(0o600, 0o600),
		(0o755, 0o644),
		(0o4700, 0o600),
	] {
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(&path, std::fs::Permissions::from_mode(host)).unwrap();
		assert_eq!(host_file_secret_mode(&path), want, "host mode {host:o}");
	}
}

#[test]
fn host_file_mode_of_a_missing_file_falls_back_to_0444() {
	assert_eq!(
		host_file_secret_mode(Path::new("/nonexistent/secret")),
		0o444
	);
}

#[test]
fn inline_content_secret_is_scoped_native_with_payload() {
	// `content:` becomes a project-scoped native secret carrying the bytes;
	// the mount target defaults to the bare compose name (→ /run/secrets/tok).
	let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    content: supersecret\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].source, "proj_secret_tok");
	assert_eq!(p[0].target, "tok");
	assert_eq!(inline_bytes(&p[0]), Some(b"supersecret".as_slice()));
}

#[test]
fn inline_content_config_is_scoped_native_with_absolute_target() {
	// Configs default to an absolute container-root path.
	let p = plans("services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    content: key=value\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].source, "proj_config_cfg");
	assert_eq!(p[0].target, "/cfg");
	assert_eq!(inline_bytes(&p[0]), Some(b"key=value".as_slice()));
}

#[test]
fn env_secret_payload_comes_from_environment() {
	temp_env::with_var("PODUP_TEST_SECRET", Some("env-value"), || {
		let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    environment: PODUP_TEST_SECRET\n");
		assert_eq!(p.len(), 1);
		assert_eq!(p[0].source, "proj_secret_tok");
		assert_eq!(inline_bytes(&p[0]), Some(b"env-value".as_slice()));
	});
}

#[test]
fn env_secret_missing_var_errors() {
	temp_env::with_var("PODUP_TEST_MISSING", None::<&str>, || {
		let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    environment: PODUP_TEST_MISSING\n").unwrap();
		assert!(
			collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
		);
	});
}

#[test]
fn external_secret_keeps_compose_name_unscoped_no_payload() {
	// An `external: true` secret points at a pre-existing podman secret: the
	// source equals the compose name (no project scoping) and carries no
	// payload. The mount filename defaults to the compose name.
	let p = plans("services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].source, "tok");
	assert_eq!(p[0].target, "tok");
	assert!(p[0].payload.is_none());
}

#[test]
fn external_secret_long_form_maps_source_target_and_perms() {
	// A long-form ref overrides the mount name, a custom top-level `name:` is
	// the real podman secret, and numeric uid/gid/mode pass through. `mode:` is
	// octal notation per the Compose Specification (leading-zero `0400`).
	let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        target: app_tok\n        uid: \"100\"\n        gid: \"101\"\n        mode: 0400\nsecrets:\n  tok:\n    external: true\n    name: real_tok\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].source, "real_tok");
	assert_eq!(p[0].target, "app_tok");
	assert_eq!(p[0].uid, Some(100));
	assert_eq!(p[0].gid, Some(101));
	assert_eq!(p[0].mode, Some(0o400));
}

#[test]
fn external_config_becomes_native_with_absolute_default_target() {
	let p = plans("services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    external: true\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].source, "cfg");
	assert_eq!(p[0].target, "/cfg");
}

#[test]
fn non_numeric_uid_drops_to_default() {
	// libpod secret uid/gid are numeric; a user/group name falls back to the
	// default rather than erroring.
	let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        uid: appuser\nsecrets:\n  tok:\n    external: true\n");
	assert_eq!(p.len(), 1);
	assert!(p[0].uid.is_none());
}

#[test]
fn native_secret_rejects_setuid_mode() {
	// 0o4000 (= 2048) is setuid; refused before the spec reaches Podman.
	let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 2048\nsecrets:\n  tok:\n    external: true\n").unwrap();
	assert!(
		collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
	);
}

#[test]
fn native_secret_rejects_execute_mode() {
	// 0o777 (= 511) sets execute bits; a secret holds data, never code.
	let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 511\nsecrets:\n  tok:\n    external: true\n").unwrap();
	assert!(
		collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
	);
}

#[test]
fn native_config_rejects_setgid_mode() {
	// External configs share the mode guard. 0o2000 (= 1024) is setgid.
	let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    configs:\n      - source: cfg\n        mode: 1024\nconfigs:\n  cfg:\n    external: true\n").unwrap();
	assert!(
		collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
	);
}

#[test]
fn inline_secret_rejects_dangerous_mode() {
	// The mode guard also covers project-created inline secrets.
	let file = crate::compose::parse_str_raw("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 511\nsecrets:\n  tok:\n    content: data\n").unwrap();
	assert!(
		collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
	);
}

#[test]
fn native_secret_allows_world_readable_mode() {
	// 0o444 (= 292) is the Podman/compose default for an in-container secret
	// and must be allowed (unlike the old shared-host staging path).
	let p = plans("services:\n  web:\n    image: nginx\n    secrets:\n      - source: tok\n        mode: 292\nsecrets:\n  tok:\n    external: true\n");
	assert_eq!(p[0].mode, Some(0o444));
}

#[test]
fn empty_and_oversized_payloads_rejected() {
	assert!(check_secret_size("s", 0).is_err());
	assert!(check_secret_size("s", MAX_SECRET_BYTES).is_err());
	assert!(check_secret_size("s", MAX_SECRET_BYTES - 1).is_ok());
	assert!(check_secret_size("s", 1).is_ok());
}

#[test]
fn inline_secret_with_unsafe_name_is_rejected() {
	// A path-traversal / control-laden key must not become a Podman secret name.
	let file = crate::compose::parse_str_raw(
		"services:\n  web:\n    image: x\n    secrets: ['../evil']\nsecrets:\n  '../evil':\n    content: data\n",
	)
	.unwrap();
	assert!(
		collect_native_plans("proj", &file.services["web"], &file, Path::new("/base")).is_err()
	);
}

#[test]
fn native_secret_without_mode_defaults_to_0444() {
	// The Compose Specification default is world-readable 0444; a Podman-native
	// secret otherwise mounts at 0000 and a non-root container user can't read it.
	let p = plans("services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    content: data\n");
	assert_eq!(p.len(), 1);
	assert_eq!(p[0].mode, Some(0o444));
}

#[test]
fn secret_mode_leading_zero_is_octal() {
	// `0444` (leading-zero octal, the Compose Specification spelling) parses as
	// octal 0o444, not decimal 444 (which would fail) — issue #2.
	let p = plans("services:\n  web:\n    image: x\n    secrets:\n      - source: tok\n        mode: 0444\nsecrets:\n  tok:\n    content: data\n");
	assert_eq!(p[0].mode, Some(0o444));
}

#[test]
fn is_podup_created_source_classifies_sources() {
	assert!(is_podup_created_source(None, Some("x"), None, None));
	assert!(is_podup_created_source(None, None, Some("VAR"), None));
	// A `file:` source is created by podup too, so `down` must remove it.
	assert!(is_podup_created_source(None, None, None, Some("./tok.txt")));
	assert!(!is_podup_created_source(Some(true), Some("x"), None, None));
	assert!(!is_podup_created_source(None, None, None, None));
}
