//! Log-rotation rendering tests for the Quadlet export.
//!
//! `internal/quadlet/tests/fields.rs` carries the rest of the field-mapping
//! coverage; the log-rotation tests are pulled out into this file because
//! `fields.rs` was already near the per-file line cap and these two tests
//! alone were enough to push it over (#1354).

use super::unit_named;
use crate::parse_str;
use crate::quadlet::generate_at;

/// A service with no `logging:` block gets the rotation default so the
/// generated quadlet units ship the same rotation policy that `up` would
/// apply at runtime (#1354). Without this, `generate quadlet` would emit a
/// unit that runs without rotation, diverging from `up` behaviour.
#[test]
fn logging_default_is_emitted_when_logging_block_is_absent() {
	let yaml = r#"
services:
  s:
    image: x
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("LogDriver=k8s-file"),
		"missing default LogDriver in:\n{c}"
	);
	assert!(
		c.contains("LogOpt=max-size=10m"),
		"missing max-size LogOpt in:\n{c}"
	);
	assert!(
		c.contains("LogOpt=max-file=5"),
		"missing max-file LogOpt in:\n{c}"
	);
}

/// An explicit `logging:` block overrides the default — the rendered unit
/// carries the user's driver and options, not the default (#1354).
#[test]
fn logging_user_override_replaces_the_default() {
	let yaml = r#"
services:
  s:
    image: x
    logging:
      driver: journald
      options:
        tag: mytag
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-s.container").contents;
	assert!(
		c.contains("LogDriver=journald"),
		"user override not applied:\n{c}"
	);
	assert!(
		c.contains("LogOpt=tag=mytag"),
		"user options not applied:\n{c}"
	);
	assert!(
		!c.contains("max-size=10m"),
		"default leaked through override:\n{c}"
	);
	assert!(
		!c.contains("max-file=5"),
		"default leaked through override:\n{c}"
	);
}
