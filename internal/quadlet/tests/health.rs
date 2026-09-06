//! Quadlet rendering of the `healthcheck:` block, including the Podman
//! extensions that have no Compose Specification equivalent.
//!
//! Split from `fields.rs` to keep that file within the source line limit.

use super::unit_named;
use crate::parse_str;
use crate::quadlet::generate_at;

/// #1095: the `x-podman-on-failure` extension reaches the Quadlet unit as
/// `HealthOnFailure=`, so `generate quadlet` and `autostart --mode quadlet`
/// carry it too, not just the live `up` path.
#[test]
fn health_on_failure_extension_reaches_the_quadlet_unit() {
	let yaml = r#"
services:
  app:
    image: x
    healthcheck:
      test: ["CMD", "true"]
      x-podman-on-failure: restart
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-app.container").contents;
	assert!(c.contains("HealthOnFailure=restart"), "{c}");
}

/// An invalid value warns instead of being emitted. Quadlet drops the whole unit
/// at daemon-reload on an unrecognised key, which is a far worse failure than
/// the key being absent, and generation has no error channel to refuse in.
#[test]
fn an_invalid_health_on_failure_warns_rather_than_emitting() {
	let yaml = r#"
services:
  app:
    image: x
    healthcheck:
      test: ["CMD", "true"]
      x-podman-on-failure: bogus
"#;
	let file = parse_str(yaml).unwrap();
	let out = generate_at(&file, "p", std::path::Path::new("/srv/app"));
	let c = &unit_named(&out, "p-app.container").contents;
	assert!(
		!c.contains("HealthOnFailure"),
		"must not emit a bad key: {c}"
	);
	assert!(
		out.warnings.iter().any(|w| w.contains("bogus")),
		"expected a warning naming the bad value: {:?}",
		out.warnings
	);
}
