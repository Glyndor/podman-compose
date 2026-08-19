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
///
/// The cap is rendered as a byte count rather than `10m` since #1417 moved
/// it into libpod's typed `size` field; `podman run --log-opt
/// max-size=10485760` was measured to produce the same cap as `max-size=10m`
/// on Podman 5.7.0. `max-file` is deliberately absent: it was measured to be
/// dropped by both the API and the CLI, so emitting it promised a rotation
/// nothing performed.
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
		c.contains("LogOpt=max-size=10485760"),
		"missing max-size LogOpt in:\n{c}"
	);
	assert!(
		!c.contains("max-file"),
		"max-file is honoured by neither the API nor the CLI; emitting it \
		 promises a rotation nothing performs:\n{c}"
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
		!c.contains("max-size"),
		"default leaked through override:\n{c}"
	);
}
