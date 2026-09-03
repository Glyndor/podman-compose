use podup::parse_str;

use crate::audit;

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

// Pull the names this test file reaches for into scope so the body reads as
// straight assertions rather than naming the path every line.
#[allow(unused_imports)]
use super::{
	audit_file, ordered_services, render_json, render_table, AuditReport, Finding as _, Finding,
};

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

/// Parse `yaml` and return the only service. Panics if zero or >1, every
/// check test targets a single-service file. Kept private to the module so
/// no other file grows a dependency on this shape.
fn single_service(yaml: &str) -> (String, podup::compose::types::Service) {
	let file = parse_str(yaml).expect("compose parses");
	let mut iter = file.services.into_iter();
	let (name, svc) = iter.next().expect("at least one service");
	assert!(iter.next().is_none(), "test should target one service");
	(name, svc)
}

/// Find a finding for `service` whose check id equals `check`. Returns `None`
/// when no such finding exists. Many tests use this as a list
/// non-membership test (the early return they wanted was "no finding", the
/// absence is the assertion).
fn has_check(report: &AuditReport, service: &str, check: &str) -> bool {
	report
		.findings
		.iter()
		.any(|f| f.service == service && f.check == check)
}

// ---------------------------------------------------------------------------
// Top-level run sanity: a fully hardened service has no findings.
// ---------------------------------------------------------------------------

#[test]
fn audit_fully_hardened_service_has_no_findings() {
	// The issue spells the list of keys a hardened service needs; this
	// combines every one of them so a single regression in any check flips
	// this test red.
	let yaml = r#"
services:
  web:
    image: nginx:1.27@sha256:0e7bb5afc7e5e22ee46c4f2cd4a8b3fa63ad3f5d5e5e5e5e5e5e5e5e5e5e5e5e
    read_only: true
    cap_drop: [ALL]
    security_opt: [no-new-privileges:true]
    pids_limit: 200
    mem_limit: 512m
    userns_mode: auto
    environment:
      - LEVEL=info
      - HOSTNAME
"#;
	let (name, svc) = single_service(yaml);
	let file = parse_str(yaml).unwrap();
	let report = audit::audit_file(&file);
	let findings_for_web: Vec<&Finding> = report
		.findings
		.iter()
		.filter(|f| f.service == name)
		.collect();
	assert!(
		findings_for_web.is_empty(),
		"hardened service produced findings: {findings_for_web:#?}"
	);
	// Sanity: the service really did carry every key we wanted to test, so
	// the empty finding list reflects the check passing each one rather than
	// the checks having nothing to look at. The cap_drop presence in
	// particular guards the most-frequently-regressed check.
	assert!(
		svc.read_only == Some(true),
		"sanity: read_only must be true"
	);
	assert!(
		svc.cap_drop.iter().any(|c| c == "ALL"),
		"sanity: cap_drop must contain ALL"
	);
	assert!(
		!svc.security_opt.is_empty(),
		"sanity: security_opt must be set"
	);
	assert!(svc.pids_limit.is_some(), "sanity: pids_limit must be set");
	assert!(svc.mem_limit.is_some(), "sanity: mem_limit must be set");
	assert!(svc.userns_mode.is_some(), "sanity: userns_mode must be set");
}

// ---------------------------------------------------------------------------
// secret_in_environment: bare keys and ${VAR} placeholders must NOT fire.
// ---------------------------------------------------------------------------

#[test]
fn audit_secret_in_environment_ignores_bare_keys_and_placeholders() {
	// Three environment entries that *look* like secrets by name but carry
	// no value at all. The check deliberately leaves these alone, a bare
	// key inherits from the host and a ${VAR} placeholder is still
	// unresolved, neither of which is a published secret.
	let yaml = r#"
services:
  app:
    image: alpine:3.20
    environment:
      - DB_PASSWORD
      - API_TOKEN=${TOKEN_FROM_ENV}
"#;
	let file = parse_str(yaml).expect("parses");
	let report = audit::audit_file(&file);
	assert!(
		!has_check(&report, "app", "secret_in_environment"),
		"bare / placeholder secrets must not fire: {:#?}",
		report.findings
	);
	// And the same shape via the map form, so a regression in the List arm
	// doesn't pass this test silently. `null` is what compose / podman read
	// as "inherit from the host", the parser turns it into a `None` value
	// in the `to_map()` view.
	let yaml_map = r#"
services:
  app:
    image: alpine:3.20
    environment:
      DB_PASSWORD: null
      API_TOKEN: ${TOKEN_FROM_ENV}
"#;
	let file = parse_str(yaml_map).expect("parses");
	let report = audit::audit_file(&file);
	assert!(
		!has_check(&report, "app", "secret_in_environment"),
		"map-form bare / placeholder secrets must not fire: {:#?}",
		report.findings
	);
}

// ---------------------------------------------------------------------------
// unpinned_image: a digest always counts as pinned.
// ---------------------------------------------------------------------------

#[test]
fn audit_unpinned_image_accepts_a_digest() {
	// The image reference carries an explicit tag *and* a digest. The
	// digest is the actual pin; the tag is a convenience. A regression
	// that ignores `@sha256:` and looks at the tag would flag this
	// reference (the tag is `latest`, no less) and the test flips red.
	let yaml = r#"
services:
  app:
    image: nginx:latest@sha256:0e7bb5afc7e5e22ee46c4f2cd4a8b3fa63ad3f5d5e5e5e5e5e5e5e5e5e5e5e5e
"#;
	let file = parse_str(yaml).expect("parses");
	let report = audit::audit_file(&file);
	assert!(
		!has_check(&report, "app", "unpinned_image"),
		"digest-pinned image must not be flagged: {:#?}",
		report.findings
	);
	// Also cover the no-tag case with a digest, so an `rfind` regression
	// on the colon/slash boundary gets caught too.
	let yaml_notag = r#"
services:
  app:
    image: nginx@sha256:0e7bb5afc7e5e22ee46c4f2cd4a8b3fa63ad3f5d5e5e5e5e5e5e5e5e5e5e5e5e
"#;
	let report = audit::audit_file(&parse_str(yaml_notag).unwrap());
	assert!(
		!has_check(&report, "app", "unpinned_image"),
		"digest-only image must not be flagged"
	);
}

/// `has_findings` is what `--strict` reads; a mutation sweep replaced it with
/// a constant and only the binary tests noticed.
#[test]
fn audit_report_has_findings_follows_the_list() {
	let clean = report_for_file("services:\n  web:\n    image: alpine:3.20\n    read_only: true\n    cap_drop: [ALL]\n    security_opt: [no-new-privileges:true]\n    pids_limit: 64\n    mem_limit: 64m\n    userns_mode: auto\n");
	assert!(!clean.has_findings());
	let dirty = report_for_file("services:\n  web:\n    image: alpine\n");
	assert!(dirty.has_findings());
}

/// `by_service` hands each service exactly its own findings, in file order,
/// including a service with none.
#[test]
fn audit_report_by_service_keeps_file_order_and_ownership() {
	let file = parse_str(
		"services:\n  zeta:\n    image: alpine:3.20\n    privileged: true\n  alpha:\n    image: alpine:3.20\n    read_only: true\n    cap_drop: [ALL]\n    security_opt: [no-new-privileges:true]\n    pids_limit: 64\n    mem_limit: 64m\n    userns_mode: auto\n",
	)
	.unwrap();
	let report = audit_file(&file);
	let services = ordered_services(&file);
	assert_eq!(
		services.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
		vec!["zeta", "alpha"]
	);
	let grouped = report.by_service(&services);
	assert_eq!(grouped.len(), 2);
	assert_eq!(grouped[0].0, "zeta");
	assert!(grouped[0].1.iter().all(|f| f.service == "zeta"));
	assert!(grouped[0].1.iter().any(|f| f.check == "privileged"));
	assert_eq!(grouped[1].0, "alpha");
	assert!(grouped[1].1.is_empty());
}

/// A value that merely ends in `}` is not a `${VAR}` placeholder.
#[test]
fn audit_secret_in_environment_flags_a_value_that_only_ends_in_a_brace() {
	let report = report_for_file("services:\n  web:\n    image: alpine:3.20\n    environment:\n      DB_PASSWORD: \"abc}\"\n");
	assert!(report
		.findings
		.iter()
		.any(|f| f.check == "secret_in_environment"));
}

fn report_for_file(yaml: &str) -> AuditReport {
	audit_file(&parse_str(yaml).unwrap())
}
