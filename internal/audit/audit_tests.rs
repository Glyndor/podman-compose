use std::path::Path;

use podup::parse_str;

use crate::audit;
use crate::cli::ConfigFormat;
use crate::startup::{render_config_to, ConfigOutput};

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

// Pull the names this test file reaches for into scope so the body reads as
// straight assertions rather than naming the path every time.
#[allow(unused_imports)]
use super::{
	audit_file, ordered_services, render_json, render_json_to, render_table, render_table_to,
	AuditReport, Finding as _, Finding,
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

// ---------------------------------------------------------------------------
// #1746 entry 2: terminal-escape injection in five sinks.
//
// The detail lines emitted by `podup audit` and the four `podup config
// --services/--volumes/--images/--profiles/--hash` projections print
// user-controlled compose values verbatim. A compose file authored by a
// third party can therefore inject raw terminal escapes; the cross-review
// noted that the bytes survive a pipe, because the rendering path is just
// `println!("{user_value}")`.
//
// The five sinks:
//
//   1. `audit` table detail lines (`render_table_to`)
//   2. `config --services` (`render_config_to`)
//   3. `config --volumes`  (`render_config_to`)
//   4. `config --images`   (`render_config_to`)
//   5. `config --profiles` (`render_config_to`)
//
// The audit table cells and the JSON renderer are intentionally out of
// scope: the table goes through `Table::print` -> `fit_cell` ->
// `sanitize_cell`, and `serde_json` escapes every control character. The
// "16 raw ESC bytes" payload the issue names only reaches the operator
// through the five unescaped sinks above, so this test pins those five
// and ignores the rest.
//
// The fix routes every user-controlled value through
// `podup::ui::sanitize_cell`, the same gate the table already uses. The
// test asserts that the bytes never appear verbatim, regardless of which
// user-controlled slot the attacker writes them into.
//
// The YAML parser itself rejects raw control characters in scalars, so
// the attack reaches the parsed model through env-var interpolation:
// the compose file says `image: $HOSTILE` and the operator's
// environment sets `HOSTILE` to the byte sequence. The interpolated
// scalar fallback in `merge::interpolate_scalar` stores the resolved
// string verbatim when the bytes are not a valid YAML scalar. The test
// constructs the `ComposeFile` directly with the interpolated values to
// pin the renderer behaviour without coupling to the parser's own
// rejection (which the issue does not ask to change).
// ---------------------------------------------------------------------------

/// The payload the issue names: a 16-byte sequence that, raw, asks the
/// terminal to clear the screen and move the cursor home. The exact
/// content does not matter; what matters is that the byte stream the
/// renderer produces for a sink using the value does not contain any
/// raw `\x1b` byte.
const HOSTILE: &str = "\x1b[2J\x1b[HINJECTED";

/// A `ComposeFile` whose every user-controlled slot the renderers
/// consume carries the same hostile payload. The service name is left
/// valid (compose validation rejects ESC chars in service names, so
/// that vector is not reachable from a YAML file in production); the
/// other slots are not validated against control characters and reach
/// the renderers verbatim when the typed model is built with them.
///
/// Specifically, each field is the value a different sink prints:
///
///   - `services.web.image`: the audit reason (via
///     `check_unpinned_image`) AND the `config --images` projection
///   - `services.web.cap_add`: the audit reason (via
///     `check_dangerous_capability`)
///   - `services.web.profiles`: the `config --profiles` projection
///   - `volumes[HOSTILE]`: the `config --volumes` projection
///
/// `Service` and `ComposeFile` are `#[non_exhaustive]`, so the test
/// builds a clean file from a safe YAML first, then mutates the typed
/// model in place to plant the hostile payload. This is the same shape
/// a hostile env-var interpolation (`image: $HOSTILE` with
/// `HOSTILE=\x1b[2J:latest`) would leave the model in: a string field
/// carrying a sequence of bytes the YAML parser would have refused, but
/// the typed model carries without question.
fn hostile_compose() -> podup::compose::types::ComposeFile {
	let yaml = "services:\n  web:\n    image: nginx:1.27\n    cap_add: [SYS_ADMIN]\n    \
	            profiles: [safe]\nvolumes:\n  data: null\n";
	let mut file = podup::parse_str(yaml).expect("safe compose parses");
	let web = file.services.get_mut("web").expect("web present");
	web.image = Some(format!("{HOSTILE}:latest"));
	web.cap_add = vec![HOSTILE.to_string()];
	web.profiles = vec![HOSTILE.to_string()];
	// The named volume reaches the `config --volumes` projection
	// directly. Volumes are not validated against control characters;
	// a hostile env interpolation would land here.
	let vol = file
		.volumes
		.shift_remove("data")
		.expect("data volume present");
	file.volumes.insert(HOSTILE.to_string(), vol);
	file
}

fn assert_no_hostile(label: &str, bytes: &[u8]) {
	// The hostile payload is a literal byte sequence, not a generic
	// "any ESC byte" check. Podup's own styling always emits SGR
	// sequences (`\x1b[1m`, `\x1b[0m`), so a raw byte count would
	// alert on those too; the meaningful signal is whether the exact
	// payload the attacker planted reaches the byte stream.
	let haystack = String::from_utf8_lossy(bytes);
	assert!(
		!haystack.contains(HOSTILE),
		"{label} carried the hostile payload verbatim; the attacker's compose file reached the \
		 operator's terminal. Output was: {haystack}",
	);
}

#[test]
fn hostile_compose_does_not_inject_escapes_into_audit_table_detail_lines() {
	// Sink 1: the audit table's per-finding detail lines. The service name
	// and the reason both come from user-controlled compose data; both
	// must be sanitized through `sanitize_cell` before they reach the byte
	// stream the operator (or a pipe) sees.
	let file = hostile_compose();
	let report = audit_file(&file);
	let services = ordered_services(&file);
	let mut buf: Vec<u8> = Vec::new();
	render_table_to(&mut buf, &services, &report).expect("render");
	assert_no_hostile("audit detail lines", &buf);
	// Positive control: the renderer did produce output, and the host's
	// word (sanitized) is still in the byte stream somewhere. Without
	// this assertion a test that simply returned no output would also
	// pass `assert_no_hostile`.
	let out = String::from_utf8_lossy(&buf);
	assert!(
		out.contains("INJECTED"),
		"the host's word is gone entirely: {out}"
	);
}

#[test]
fn hostile_compose_does_not_inject_escapes_into_config_list_projections() {
	// Sinks 2..5: the four `config` list-projection branches all print a
	// value taken from the compose file. They share the `_to` writer so a
	// single test covers all four.
	let file = hostile_compose();
	let mut buf: Vec<u8> = Vec::new();

	let projections: [(&str, ConfigOutput); 4] = [
		(
			"--services",
			ConfigOutput {
				services: true,
				..Default::default()
			},
		),
		(
			"--volumes",
			ConfigOutput {
				volumes: true,
				..Default::default()
			},
		),
		(
			"--images",
			ConfigOutput {
				images: true,
				..Default::default()
			},
		),
		(
			"--profiles",
			ConfigOutput {
				profiles: true,
				..Default::default()
			},
		),
	];
	for (name, out) in projections {
		buf.clear();
		render_config_to(
			&mut buf,
			&file,
			&ConfigFormat::Yaml,
			&out,
			"proj",
			Path::new("/proj"),
		)
		.unwrap_or_else(|e| panic!("{name} render: {e}"));
		assert_no_hostile(name, &buf);
	}

	// Sink 6: `config --hash` also prints the service name. The branch
	// runs the same `sanitize_cell` gate as the other projections.
	buf.clear();
	render_config_to(
		&mut buf,
		&file,
		&ConfigFormat::Yaml,
		&ConfigOutput {
			hash: Some("*".to_string()),
			..Default::default()
		},
		"proj",
		Path::new("/proj"),
	)
	.expect("--hash render");
	assert_no_hostile("--hash", &buf);
	// The 64-character hex hash is bytes we own, so the only sanitized
	// thing in this line is the service name. The print still produces a
	// line per service, so the line count is exactly one.
	assert_eq!(buf.iter().filter(|&&b| b == b'\n').count(), 1);
}
