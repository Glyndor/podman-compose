//! Unit tests for the service-level `x-podman-autoupdate` extension.

use super::{AutoUpdate, Service, X_PODMAN_AUTOUPDATE};
use indexmap::IndexMap;

fn parse_service(yaml: &str) -> Service {
	let file: crate::compose::types::ComposeFile =
		serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("failed to parse yaml: {e}\n{yaml}"));
	file.services
		.into_values()
		.next()
		.expect("expected at least one service in the yaml")
}

#[test]
fn x_podman_autoupdate_parses_registry_and_local() {
	for (raw, want) in [
		("registry", AutoUpdate::Registry),
		("local", AutoUpdate::Local),
	] {
		let yaml = format!("services:\n  web:\n    image: x\n    {X_PODMAN_AUTOUPDATE}: {raw}\n");
		let svc = parse_service(&yaml);
		assert_eq!(svc.podman_autoupdate().unwrap(), Some(want), "{raw}");

		let round_tripped = serde_yaml::to_string(&svc).unwrap();
		assert!(
			round_tripped.contains(X_PODMAN_AUTOUPDATE),
			"{round_tripped}"
		);
		assert!(
			round_tripped.contains(raw),
			"value {raw} did not survive a round trip: {round_tripped}"
		);
	}
}

#[test]
fn x_podman_autoupdate_rejects_any_other_value_naming_the_allowed_ones() {
	let yaml = format!("services:\n  web:\n    image: x\n    {X_PODMAN_AUTOUPDATE}: always\n");
	let svc = parse_service(&yaml);
	let err = svc
		.podman_autoupdate()
		.expect_err("always must not be accepted");
	assert!(
		err.contains("always"),
		"the offending value must be named in the error: {err}"
	);
	assert!(
		err.contains("registry") && err.contains("local"),
		"the allowed spellings must both be named in the error: {err}"
	);

	// A non-string value lives in `unknown` as a typed YAML value; the accessor
	// is what rejects it, naming the key and the two allowed spellings.
	let yaml = format!("services:\n  web:\n    image: x\n    {X_PODMAN_AUTOUPDATE}: 1\n");
	let svc = parse_service(&yaml);
	let err = svc
		.podman_autoupdate()
		.expect_err("a non-string value must be rejected at access time");
	let msg = err.to_string();
	assert!(
		msg.contains(X_PODMAN_AUTOUPDATE),
		"a non-string value must surface a message naming the key: {msg}"
	);
	assert!(
		msg.contains("registry") && msg.contains("local"),
		"the non-string error must also name the allowed spellings: {msg}"
	);
}

#[test]
fn x_podman_autoupdate_is_absent_by_default() {
	let svc: Service = serde_yaml::from_str("{image: x}").unwrap();
	assert_eq!(svc.podman_autoupdate().unwrap(), None);
}

#[test]
fn x_podman_autoupdate_skips_the_unknown_key_diagnostic() {
	// The diagnostics pass skips any captured key starting with `x-`, so the
	// extension is invisible to it. The key still lands in `unknown` because
	// the typed Service struct has no field for it, that is how the accessor
	// reads it, but no "unknown key" warning is emitted for it.
	let yaml = format!("services:\n  web:\n    image: x\n    {X_PODMAN_AUTOUPDATE}: registry\n");
	let svc = parse_service(&yaml);
	let diagnostics = crate::compose::collect_diagnostics(&crate::compose::types::ComposeFile {
		services: std::iter::once(("web".to_string(), svc)).collect(),
		..crate::compose::types::ComposeFile::default()
	});
	let unknown_warnings: Vec<&String> = diagnostics
		.iter()
		.filter(|w| w.contains(X_PODMAN_AUTOUPDATE))
		.collect();
	assert!(
		unknown_warnings.is_empty(),
		"x- extensions must not trigger the unknown-key diagnostic: {unknown_warnings:?}"
	);
}

/// The key round-trips through `config`: it lands in `unknown` only because
/// there is no typed field, but re-serializing the service keeps it. A dropped
/// extension would make `config` output that no longer does what the input
/// did.
#[test]
fn x_podman_autoupdate_survives_a_round_trip() {
	let yaml = format!("services:\n  web:\n    image: x\n    {X_PODMAN_AUTOUPDATE}: registry\n");
	let svc = parse_service(&yaml);
	let out = serde_yaml::to_string(&svc).unwrap();
	assert!(out.contains(X_PODMAN_AUTOUPDATE), "{out}");
	assert!(out.contains("registry"), "{out}");
}

/// Constructing an `unknown` map by hand and reading it through the accessor
/// must work, that path is what the integration tests use when they build a
/// service without round-tripping through YAML.
#[test]
fn x_podman_autoupdate_reads_from_a_hand_built_unknown_map() {
	let mut unknown: IndexMap<String, serde_yaml::Value> = IndexMap::new();
	unknown.insert(
		X_PODMAN_AUTOUPDATE.to_string(),
		serde_yaml::Value::String("local".to_string()),
	);
	let svc = Service {
		image: Some("x".to_string()),
		unknown,
		..Service::default()
	};
	assert_eq!(svc.podman_autoupdate().unwrap(), Some(AutoUpdate::Local));
}

#[test]
fn auto_update_as_str_matches_podman_spelling() {
	assert_eq!(AutoUpdate::Registry.as_str(), "registry");
	assert_eq!(AutoUpdate::Local.as_str(), "local");
}
