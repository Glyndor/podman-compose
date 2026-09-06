//! Tests for the `extends` / multi-`-f` service merge.
//!
//! Split out of `merge.rs` to keep that file within the source line limit.

use crate::parse_str;

#[test]
fn extends_unions_sequence_fields() {
	let yaml = r#"
services:
  base:
    image: alpine
    ports:
      - "80:80"
      - "81:81"
  app:
    extends: base
    ports:
      - "90:90"
"#;
	let file = parse_str(yaml).unwrap();
	// Compose `extends` combines sequences (base first, then the extending
	// service's items) rather than replacing the base wholesale.
	assert_eq!(file.services["app"].ports.len(), 3);
}

#[test]
fn extends_dedups_identical_sequence_entries() {
	let yaml = r#"
services:
  base:
    image: alpine
    ports:
      - "80:80"
  app:
    extends: base
    ports:
      - "80:80"
      - "90:90"
"#;
	let file = parse_str(yaml).unwrap();
	// An exact duplicate from the extending service is dropped.
	assert_eq!(file.services["app"].ports.len(), 2);
}

#[test]
fn absent_list_field_falls_back_to_base() {
	let yaml = r#"
services:
  base:
    image: alpine
    ports:
      - "80:80"
  app:
    extends: base
"#;
	let file = parse_str(yaml).unwrap();
	assert_eq!(file.services["app"].ports.len(), 1);
}

#[test]
fn labels_are_merged_with_override_winning() {
	let yaml = r#"
services:
  base:
    image: alpine
    labels:
      a: base
      keep: base
  app:
    extends: base
    labels:
      a: over
      b: over
"#;
	let file = parse_str(yaml).unwrap();
	let labels = file.services["app"].labels.to_map();
	assert_eq!(labels.get("a").map(|s| s.as_str()), Some("over"));
	assert_eq!(labels.get("keep").map(|s| s.as_str()), Some("base"));
	assert_eq!(labels.get("b").map(|s| s.as_str()), Some("over"));
}

#[test]
fn empty_override_keeps_base_depends_on() {
	let yaml = r#"
services:
  db:
    image: postgres
  base:
    image: alpine
    depends_on:
      - db
  app:
    extends: base
"#;
	let file = parse_str(yaml).unwrap();
	assert_eq!(
		file.services["app"].depends_on.service_names(),
		vec!["db".to_string()]
	);
}

#[test]
fn extends_unions_depends_on() {
	let yaml = r#"
services:
  db:
    image: postgres
  cache:
    image: redis
  base:
    image: alpine
    depends_on:
      - db
  app:
    extends: base
    depends_on:
      - cache
"#;
	let file = parse_str(yaml).unwrap();
	// compose-go unions the base and extending depends_on rather than letting
	// the override replace the base wholesale.
	let mut names = file.services["app"].depends_on.service_names();
	names.sort();
	assert_eq!(names, vec!["cache".to_string(), "db".to_string()]);
}

#[test]
fn extends_depends_on_override_wins_on_conflict() {
	let yaml = r#"
services:
  db:
    image: postgres
  base:
    image: alpine
    depends_on:
      db:
        condition: service_started
  app:
    extends: base
    depends_on:
      db:
        condition: service_healthy
"#;
	let file = parse_str(yaml).unwrap();
	// On an overlapping key the extending service's condition wins.
	assert_eq!(
		file.services["app"].depends_on.condition_for("db"),
		crate::compose::types::ServiceCondition::ServiceHealthy
	);
}

#[test]
fn absent_override_keeps_base_environment() {
	let yaml = r#"
services:
  base:
    image: alpine
    environment:
      A: "1"
  app:
    extends: base
"#;
	let file = parse_str(yaml).unwrap();
	let env = file.services["app"].environment.to_map();
	assert_eq!(env.get("A").and_then(|v| v.clone()).as_deref(), Some("1"));
}

/// #1078: `dns` and its siblings are appended, not replaced. docker compose
/// concatenates these sequences; replacing meant an override adding one
/// nameserver silently removed every other one.
#[test]
fn scalar_or_list_field_override_appends_to_base() {
	let yaml = r#"
services:
  base:
    image: alpine
    dns:
      - 1.1.1.1
  app:
    extends: base
    dns:
      - 9.9.9.9
"#;
	let file = parse_str(yaml).unwrap();
	assert_eq!(
		file.services["app"].dns.to_list(),
		vec!["1.1.1.1", "9.9.9.9"],
		"the base's nameserver must survive an override that adds one"
	);
}

/// #1078: `env_file` is appended too: the base's files are still read, in
/// order, followed by the override's.
#[test]
fn env_file_override_appends_to_base() {
	let yaml = r#"
services:
  base:
    image: alpine
    env_file:
      - base.env
  app:
    extends: base
    env_file:
      - app.env
"#;
	let file = parse_str(yaml).unwrap();
	let entries = file.services["app"].env_file.to_entries();
	assert_eq!(entries.len(), 2, "both env files must be read");
	assert_eq!(entries[0].path(), "base.env");
	assert_eq!(entries[1].path(), "app.env");
}

#[test]
fn depends_on_unions_when_base_has_none() {
	// The base declares no depends_on; the extending service's dependencies are
	// carried through unchanged (merge_depends_on base-empty branch).
	let yaml = r#"
services:
  base:
    image: alpine
  db:
    image: postgres
  app:
    extends: base
    depends_on:
      - db
"#;
	let file = parse_str(yaml).unwrap();
	assert!(file.services["app"]
		.depends_on
		.service_names()
		.contains(&"db".to_string()));
}

/// #1078, the one the issue calls worse than a wrong value: a service on
/// `backend` in the base and `monitoring` in the override silently lost
/// `backend`. It dropped off the network and service discovery failed at run
/// time, far from the config that caused it.
#[test]
fn networks_are_unioned_not_replaced() {
	let yaml = r#"
services:
  base:
    image: alpine
    networks:
      - backend
  app:
    extends: base
    networks:
      - monitoring
networks:
  backend:
  monitoring:
"#;
	let file = parse_str(yaml).unwrap();
	let names = file.services["app"].networks.names();
	assert!(
		names.contains(&"backend".to_string()),
		"the base's network must survive: {names:?}"
	);
	assert!(
		names.contains(&"monitoring".to_string()),
		"the override's network must be added: {names:?}"
	);
}

/// A bare name in the override must not erase per-network config the base
/// set: the union keeps the config unless the override supplies its own.
#[test]
fn network_union_keeps_base_config_for_a_bare_override_entry() {
	let yaml = r#"
services:
  base:
    image: alpine
    networks:
      backend:
        aliases:
          - db
  app:
    extends: base
    networks:
      - backend
networks:
  backend:
"#;
	let file = parse_str(yaml).unwrap();
	let cfg = file.services["app"].networks.config_for("backend");
	assert!(
		cfg.is_some_and(|c| c
			.aliases
			.as_ref()
			.is_some_and(|a| a.contains(&"db".to_string()))),
		"a bare override entry must not wipe the base's aliases"
	);
}

/// #1078: `sysctls` merges per key like `environment` and `labels`, rather
/// than the override replacing the whole map.
#[test]
fn sysctls_merge_per_key() {
	let yaml = r#"
services:
  base:
    image: alpine
    sysctls:
      net.core.somaxconn: "1024"
      net.ipv4.tcp_syncookies: "1"
  app:
    extends: base
    sysctls:
      net.core.somaxconn: "4096"
"#;
	let file = parse_str(yaml).unwrap();
	let m = file.services["app"].sysctls.to_map();
	assert_eq!(
		m.get("net.core.somaxconn").map(String::as_str),
		Some("4096"),
		"the override wins for a key both set"
	);
	assert_eq!(
		m.get("net.ipv4.tcp_syncookies").map(String::as_str),
		Some("1"),
		"a key only the base sets must survive"
	);
}

/// The union must survive serialization: `config` renders the merged service
/// back to YAML, and a map whose values are all `None` must still emit its
/// network names rather than an empty block.
#[test]
fn unioned_networks_serialize_back_to_their_names() {
	let yaml = r#"
services:
  base:
    image: alpine
    networks:
      - backend
  app:
    extends: base
    networks:
      - monitoring
networks:
  backend:
  monitoring:
"#;
	let file = parse_str(yaml).unwrap();
	let rendered = serde_yaml::to_string(&file.services["app"].networks).unwrap();
	assert!(
		rendered.contains("backend") && rendered.contains("monitoring"),
		"serialized form lost the network names: {rendered}"
	);
}

/// #1078: a base and an override mounting different sources at the *same*
/// target used to emit both, and `podman create` refused the container with
/// `duplicate mount destination`. Remapping a path in an override is exactly
/// what the override is for, so the later one wins.
#[test]
fn volumes_at_the_same_target_are_replaced_not_duplicated() {
	let yaml = r#"
services:
  base:
    image: alpine
    volumes:
      - ./a:/data
      - ./logs:/var/log
  app:
    extends: base
    volumes:
      - ./b:/data
"#;
	let file = parse_str(yaml).unwrap();
	let mounts = &file.services["app"].volumes;
	let at_data: Vec<_> = mounts.iter().filter(|m| m.target() == "/data").collect();
	assert_eq!(at_data.len(), 1, "one mount per target: {mounts:?}");
	assert!(
		format!("{:?}", at_data[0]).contains("./b"),
		"the override's source must win: {:?}",
		at_data[0]
	);
	// A target only the base declares is untouched.
	assert!(
		mounts.iter().any(|m| m.target() == "/var/log"),
		"an unrelated base mount must survive: {mounts:?}"
	);
}

/// #1078: `!override` takes the overriding value whole instead of appending.
/// Both tags were accepted and silently ignored, so a file asking for
/// replacement got a merge, the opposite of what it said.
#[test]
fn override_tag_replaces_instead_of_appending() {
	let base =
		parse_str("services:\n  web:\n    image: alpine\n    ports: [\"8080:80\"]\n").unwrap();
	let over = parse_str("services:\n  web:\n    ports: !override [\"9090:80\"]\n").unwrap();
	let raw: serde_yaml::Value =
		serde_yaml::from_str("services:\n  web:\n    ports: !override [\"9090:80\"]\n").unwrap();
	let directives = crate::compose::tags::collect(&raw);

	let merged = crate::compose::extends::merge_service_tagged(
		base.services["web"].clone(),
		over.services["web"].clone(),
		directives.get("web"),
	);
	assert_eq!(
		merged.ports.len(),
		1,
		"!override must replace, not append: {:?}",
		merged.ports
	);
}

/// `!reset` drops the key entirely, leaving the type's default.
#[test]
fn reset_tag_clears_the_base_value() {
	let base = parse_str("services:\n  web:\n    image: alpine\n    dns: [\"1.1.1.1\"]\n").unwrap();
	let over = parse_str("services:\n  web:\n    dns: !reset []\n").unwrap();
	let raw: serde_yaml::Value =
		serde_yaml::from_str("services:\n  web:\n    dns: !reset []\n").unwrap();
	let directives = crate::compose::tags::collect(&raw);

	let merged = crate::compose::extends::merge_service_tagged(
		base.services["web"].clone(),
		over.services["web"].clone(),
		directives.get("web"),
	);
	assert!(
		merged.dns.to_list().is_empty(),
		"!reset must clear the base: {:?}",
		merged.dns
	);
}

/// Without a tag the ordinary merge rule still applies; the tags must not
/// change the default behaviour of anything they are not on.
#[test]
fn an_untagged_key_still_merges_normally() {
	let base = parse_str("services:\n  web:\n    image: alpine\n    dns: [\"1.1.1.1\"]\n").unwrap();
	let over = parse_str("services:\n  web:\n    dns: [\"9.9.9.9\"]\n").unwrap();
	let merged = crate::compose::extends::merge_service_tagged(
		base.services["web"].clone(),
		over.services["web"].clone(),
		None,
	);
	assert_eq!(merged.dns.to_list(), vec!["1.1.1.1", "9.9.9.9"]);
}
