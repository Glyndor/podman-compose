use super::{
	config_hash, resolve_links, resolve_stop_signal, resolve_volume_name, resolve_volumes_from,
};
use crate::parse_str;

#[test]
fn stop_signal_resolves_names_numbers_and_prefixes() {
	// Named, case-insensitive, with and without the SIG prefix.
	assert_eq!(resolve_stop_signal("SIGTERM").unwrap(), 15);
	assert_eq!(resolve_stop_signal("TERM").unwrap(), 15);
	assert_eq!(resolve_stop_signal("sigterm").unwrap(), 15);
	assert_eq!(resolve_stop_signal("SIGKILL").unwrap(), 9);
	assert_eq!(resolve_stop_signal("hup").unwrap(), 1);
	assert_eq!(resolve_stop_signal("SIGUSR1").unwrap(), 10);
	assert_eq!(resolve_stop_signal("SIGUSR2").unwrap(), 12);
	// Bare numbers pass through (optionally surrounded by whitespace).
	assert_eq!(resolve_stop_signal("15").unwrap(), 15);
	assert_eq!(resolve_stop_signal(" 9 ").unwrap(), 9);
}

#[test]
fn stop_signal_resolves_realtime_signals() {
	// Realtime signals and their offset forms, as docker-compose/Podman accept
	// them (images on s6-overlay/tini commonly use SIGRTMIN+3).
	assert_eq!(resolve_stop_signal("SIGRTMIN").unwrap(), 34);
	assert_eq!(resolve_stop_signal("SIGRTMIN+3").unwrap(), 37);
	assert_eq!(resolve_stop_signal("SIGRTMAX").unwrap(), 64);
	assert_eq!(resolve_stop_signal("SIGRTMAX-1").unwrap(), 63);
	// The SIG prefix is optional here too.
	assert_eq!(resolve_stop_signal("RTMIN+3").unwrap(), 37);
}

#[test]
fn stop_signal_rejects_out_of_range_realtime_offset() {
	// 34 + 40 = 74 is past SIGRTMAX (64), so it is rejected.
	let err = resolve_stop_signal("SIGRTMIN+40").unwrap_err();
	assert!(err.to_string().contains("SIGRTMIN+40"), "got: {err}");
	// Wrong-direction offsets are rejected too.
	assert!(resolve_stop_signal("SIGRTMIN-1").is_err());
	assert!(resolve_stop_signal("SIGRTMAX+1").is_err());
}

#[test]
fn stop_signal_unknown_name_errors() {
	let err = resolve_stop_signal("SIGNOPE").unwrap_err();
	assert!(err.to_string().contains("SIGNOPE"), "got: {err}");
}

#[test]
fn links_resolve_to_container_names_external_links_verbatim() {
	let file = parse_str(
		"services:\n  db:\n    image: x\n  web:\n    image: x\n    links:\n      - db\n      - db:primary\n    external_links:\n      - legacy_db:db\n",
	)
	.unwrap();
	let links = resolve_links(&file.services["web"], &file, "proj");
	// Auto-generated names are always index-suffixed; a link targets the
	// first replica.
	assert!(links.contains(&"proj-db-1:db".to_string()));
	assert!(links.contains(&"proj-db-1:primary".to_string()));
	assert!(links.contains(&"legacy_db:db".to_string()));
}

#[test]
fn links_honour_custom_container_name() {
	let file = parse_str(
		"services:\n  db:\n    image: x\n    container_name: my-db\n  web:\n    image: x\n    links:\n      - db\n",
	)
	.unwrap();
	let links = resolve_links(&file.services["web"], &file, "proj");
	assert_eq!(links, vec!["my-db:db".to_string()]);
}

#[test]
fn volumes_from_resolves_to_container_names() {
	let file = parse_str(
		"services:\n  db:\n    image: x\n  cache:\n    image: x\n    container_name: my-cache\n  web:\n    image: x\n    volumes_from:\n      - db\n      - cache\n",
	)
	.unwrap();
	let resolved = resolve_volumes_from(&file.services["web"], &file, "proj");
	// Bare service name resolves to the first replica `{project}-{service}-1`.
	assert!(resolved.contains(&"proj-db-1".to_string()));
	// An explicit container_name is honoured.
	assert!(resolved.contains(&"my-cache".to_string()));
}

#[test]
fn volumes_from_preserves_access_mode() {
	let file = parse_str(
		"services:\n  db:\n    image: x\n  web:\n    image: x\n    volumes_from:\n      - db:ro\n      - service:db:rw\n",
	)
	.unwrap();
	let resolved = resolve_volumes_from(&file.services["web"], &file, "proj");
	// The `:ro`/`:rw` suffix survives the rewrite, and the `service:` prefix
	// is stripped before resolving.
	assert!(resolved.contains(&"proj-db-1:ro".to_string()));
	assert!(resolved.contains(&"proj-db-1:rw".to_string()));
}

#[test]
fn volumes_from_passes_through_container_form_and_unknown() {
	let file = parse_str(
		"services:\n  web:\n    image: x\n    volumes_from:\n      - container:legacy-data\n      - container:legacy-data:ro\n      - missing\n",
	)
	.unwrap();
	let resolved = resolve_volumes_from(&file.services["web"], &file, "proj");
	// The `container:` form names a container outside the project verbatim.
	assert!(resolved.contains(&"legacy-data".to_string()));
	assert!(resolved.contains(&"legacy-data:ro".to_string()));
	// An unknown service is left unchanged.
	assert!(resolved.contains(&"missing".to_string()));
}

#[test]
#[cfg(unix)]
fn bind_source_resolution() {
	use super::resolve_bind_source;
	use std::path::Path;
	let base = Path::new("/srv/app");
	assert_eq!(resolve_bind_source("/abs/path", base), "/abs/path");
	assert_eq!(resolve_bind_source("./data", base), "/srv/app/./data");
	assert_eq!(resolve_bind_source("data", base), "/srv/app/data");
	// Mutating HOME via the process env races other tests under the parallel
	// runner; temp_env::with_var sets and restores it atomically.
	temp_env::with_var("HOME", Some("/home/u"), || {
		assert_eq!(resolve_bind_source("~/x", base), "/home/u/x");
		assert_eq!(resolve_bind_source("~", base), "/home/u");
	});
	// An empty source is returned verbatim (no base-dir join).
	assert_eq!(resolve_bind_source("", base), "");
}

#[test]
#[cfg(unix)]
fn bind_source_tilde_without_home_stays_literal() {
	use super::resolve_bind_source;
	use std::path::Path;
	let base = Path::new("/srv/app");
	// With no home directory resolvable, a `~`-prefixed path keeps its literal
	// form, then (being relative) is anchored to the base dir.
	temp_env::with_vars(
		[("HOME", None::<&str>), ("USERPROFILE", None::<&str>)],
		|| {
			assert_eq!(resolve_bind_source("~/x", base), "/srv/app/~/x");
			assert_eq!(resolve_bind_source("~", base), "/srv/app/~");
		},
	);
}

#[test]
fn volume_name_resolution() {
	let f = parse_str(
		"services:\n  s:\n    image: x\nvolumes:\n  data:\n  ext:\n    external: true\n  custom:\n    name: my-vol\n",
	)
	.unwrap();
	assert_eq!(
		resolve_volume_name("data", "proj", &f).unwrap(),
		"proj_data"
	);
	assert_eq!(resolve_volume_name("ext", "proj", &f).unwrap(), "ext");
	assert_eq!(resolve_volume_name("custom", "proj", &f).unwrap(), "my-vol");
	// An empty reference is an anonymous volume: no name to resolve.
	assert_eq!(resolve_volume_name("", "proj", &f).unwrap(), "");
	// A non-empty reference not declared under top-level `volumes:` is
	// rejected rather than escaping into a bare, unprefixed global volume.
	let err = resolve_volume_name("missing", "proj", &f).unwrap_err();
	assert!(err.to_string().contains("missing"), "got: {err}");
}

#[test]
fn config_hash_is_stable_and_sensitive() {
	let a = parse_str("services:\n  web:\n    image: nginx:1.27\n").unwrap();
	let b = parse_str("services:\n  web:\n    image: nginx:1.27\n").unwrap();
	let c = parse_str("services:\n  web:\n    image: nginx:1.28\n").unwrap();
	let ha = config_hash(&a.services["web"], &a).unwrap();
	let hb = config_hash(&b.services["web"], &b).unwrap();
	let hc = config_hash(&c.services["web"], &c).unwrap();
	assert_eq!(ha, hb, "same config produces the same hash");
	assert_ne!(ha, hc, "a changed image produces a different hash");
	assert_eq!(ha.len(), 64, "sha-256 hex is 64 chars");
}

#[test]
fn config_hash_tracks_inline_secret_content() {
	// Rotating an inline `content:` secret must change the hash so the
	// container is recreated to pick up the new (point-in-time) native secret.
	let a = parse_str(
		"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    content: v1\n",
	)
	.unwrap();
	let b = parse_str(
		"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    content: v2\n",
	)
	.unwrap();
	assert_ne!(
		config_hash(&a.services["web"], &a).unwrap(),
		config_hash(&b.services["web"], &b).unwrap(),
		"changed inline secret content must change the hash",
	);
}

#[test]
fn config_hash_ignores_external_secret_identity() {
	// An `external:` secret is by-reference (no payload), so it does not add
	// to the hash beyond the service's own secret list.
	let a = parse_str(
		"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n",
	)
	.unwrap();
	let b = parse_str(
		"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n    name: other\n",
	)
	.unwrap();
	// The service definition is identical; only the top-level external name
	// differs, which is resolved at attach time, not baked into the hash.
	assert_eq!(
		config_hash(&a.services["web"], &a).unwrap(),
		config_hash(&b.services["web"], &b).unwrap(),
	);
}

#[test]
fn config_hash_tracks_inline_config_content() {
	// The same recreate-on-rotation contract as inline secrets applies to
	// inline `configs:` content.
	let a = parse_str(
		"services:\n  web:\n    image: x\n    configs: [cfg]\nconfigs:\n  cfg:\n    content: a\n",
	)
	.unwrap();
	let b = parse_str(
		"services:\n  web:\n    image: x\n    configs: [cfg]\nconfigs:\n  cfg:\n    content: b\n",
	)
	.unwrap();
	assert_ne!(
		config_hash(&a.services["web"], &a).unwrap(),
		config_hash(&b.services["web"], &b).unwrap(),
		"changed inline config content must change the hash",
	);
}

#[test]
fn config_hash_tracks_environment_sourced_secret() {
	// An `environment:`-sourced secret folds the *current* value of the named
	// variable into the hash, so a change to that variable recreates.
	let file = parse_str(
		"services:\n  web:\n    image: x\n    secrets: [tok]\nsecrets:\n  tok:\n    environment: PODUP_TEST_SECRET\n",
	)
	.unwrap();
	let with_a = temp_env::with_var("PODUP_TEST_SECRET", Some("alpha"), || {
		config_hash(&file.services["web"], &file).unwrap()
	});
	let with_b = temp_env::with_var("PODUP_TEST_SECRET", Some("beta"), || {
		config_hash(&file.services["web"], &file).unwrap()
	});
	assert_ne!(
		with_a, with_b,
		"a changed environment-sourced secret value must change the hash",
	);
}

#[test]
fn config_hash_stable_despite_map_field_order() {
	// `storage_opt` is a HashMap; canonical serialisation must sort its keys
	// so the hash does not flap and trigger a spurious recreate on `up`.
	let a = parse_str(
		"services:\n  web:\n    image: x\n    storage_opt:\n      size: \"10G\"\n      foo: bar\n      baz: qux\n",
	)
	.unwrap();
	let b = parse_str(
		"services:\n  web:\n    image: x\n    storage_opt:\n      baz: qux\n      size: \"10G\"\n      foo: bar\n",
	)
	.unwrap();
	assert_eq!(
		config_hash(&a.services["web"], &a).unwrap(),
		config_hash(&b.services["web"], &b).unwrap(),
		"hash must be independent of storage_opt key order",
	);
}
