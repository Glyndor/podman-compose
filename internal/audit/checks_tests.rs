//! Per-check positive/negative tests. Each check is covered by exactly two
//! names, `<check>_flags_when_bad` and `<check>_passes_when_good`, so the
//! matrix mirrors the contract verbatim: a service with the bad shape raises
//! exactly the named finding; a service with the good shape raises nothing
//! under that check.

use podup::parse_str;

use super::*;

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Run every check against `yaml` and return the flat list of findings.
/// The checks today are all per-service, so callers only need this entry.
pub(super) fn report_for(yaml: &str) -> Vec<Finding> {
	let file = parse_str(yaml).expect("compose parses");
	crate::audit::audit_file(&file).findings
}

// ---------------------------------------------------------------------------
// privileged
// ---------------------------------------------------------------------------

#[test]
fn audit_privileged_flags_when_true() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    privileged: true
"#;
	let findings = report_for(yaml);
	let privileged: Vec<&Finding> = findings
		.iter()
		.filter(|f| f.check == "privileged")
		.collect();
	assert_eq!(
		privileged.len(),
		1,
		"exactly one privileged finding expected: {findings:#?}"
	);
	assert_eq!(privileged[0].check, "privileged");
	assert_eq!(privileged[0].service, "web");
}

#[test]
fn audit_privileged_passes_when_false_or_absent() {
	// Explicit false: still fine. Absent: also fine. The check only fires on
	// the literal `true`.
	for yaml in [
		r#"services:
  web:
    image: alpine:3.20
    privileged: false
"#,
		r#"services:
  web:
    image: alpine:3.20
"#,
	] {
		let findings = report_for(yaml);
		assert!(
			!findings.iter().any(|f| f.check == "privileged"),
			"privileged check must not fire on the good shape: {findings:#?}"
		);
	}
}

// ---------------------------------------------------------------------------
// host_namespace
// ---------------------------------------------------------------------------

#[test]
fn audit_host_namespace_flags_network_mode_host() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    network_mode: host
"#;
	let findings = report_for(yaml);
	let kinds: Vec<&'static str> = findings.iter().map(|f| f.check).collect();
	assert!(kinds.contains(&"host_namespace"), "got {kinds:?}");
}

#[test]
fn audit_host_namespace_flags_pid_ipc_uts_cgroup_userns_host() {
	// One service per bad field, so each test pins a specific field rather
	// than asserting the whole shape at once.
	for (yaml_field, expected) in [
		("pid: host", "pid"),
		("ipc: host", "ipc"),
		("uts: host", "uts"),
		("cgroup: host", "cgroup"),
		("userns_mode: host", "userns_mode"),
	] {
		let yaml = format!("services:\n  web:\n    image: alpine:3.20\n    {yaml_field}\n");
		let findings = report_for(&yaml);
		assert!(
			findings
				.iter()
				.any(|f| f.check == "host_namespace" && f.reason.contains(expected)),
			"expected host_namespace finding mentioning `{expected}`; got {findings:#?}"
		);
	}
}

#[test]
fn audit_host_namespace_passes_when_clean() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "host_namespace"),
		"baseline must not flag host_namespace: {findings:#?}"
	);
	// And a service that names a non-host namespace value is also clean.
	let yaml_service = r#"
services:
  web:
    image: alpine:3.20
    pid: service:sidecar
"#;
	let findings = report_for(yaml_service);
	assert!(
		!findings.iter().any(|f| f.check == "host_namespace"),
		"service:<name> share must not flag host_namespace: {findings:#?}"
	);
}

// ---------------------------------------------------------------------------
// dangerous_capability
// ---------------------------------------------------------------------------

#[test]
fn audit_dangerous_capability_flags_sys_admin_and_all() {
	for cap in ["SYS_ADMIN", "ALL"] {
		let yaml =
			format!("services:\n  web:\n    image: alpine:3.20\n    cap_add:\n      - {cap}\n");
		let findings = report_for(&yaml);
		assert!(
			findings.iter().any(|f| f.check == "dangerous_capability"),
			"`cap_add: [{cap}]` should flag dangerous_capability; got {findings:#?}"
		);
	}
}

#[test]
fn audit_dangerous_capability_passes_when_empty() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    cap_add: [NET_ADMIN]
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "dangerous_capability"),
		"non-dangerous cap_add must not fire: {findings:#?}"
	);
}

// ---------------------------------------------------------------------------
// writable_root
// ---------------------------------------------------------------------------

#[test]
fn audit_writable_root_flags_when_read_only_false_or_absent() {
	for yaml in [
		// absent entirely.
		r#"services:
  web:
    image: alpine:3.20
"#,
		// explicit false (the compose default).
		r#"services:
  web:
    image: alpine:3.20
    read_only: false
"#,
	] {
		let findings = report_for(yaml);
		assert!(
			findings.iter().any(|f| f.check == "writable_root"),
			"the writable_root check must fire here: {findings:#?}"
		);
	}
}

#[test]
fn audit_writable_root_passes_when_read_only_true() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    read_only: true
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "writable_root"),
		"read_only: true must pass the writable_root check: {findings:#?}"
	);
}

// ---------------------------------------------------------------------------
// no_cap_drop_all
// ---------------------------------------------------------------------------

#[test]
fn audit_no_cap_drop_all_flags_when_absent() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
"#;
	let findings = report_for(yaml);
	assert!(
		findings.iter().any(|f| f.check == "no_cap_drop_all"),
		"absent cap_drop must fire: {findings:#?}"
	);
	// cap_drop without ALL is still bad, only the literal `ALL` zeroes
	// the baseline.
	let yaml_other = r#"
services:
  web:
    image: alpine:3.20
    cap_drop: [NET_RAW, MKNOD]
"#;
	let findings = report_for(yaml_other);
	assert!(
		findings.iter().any(|f| f.check == "no_cap_drop_all"),
		"cap_drop without ALL must fire: {findings:#?}"
	);
}

#[test]
fn audit_no_cap_drop_all_passes_when_all_present() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    cap_drop: [ALL]
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "no_cap_drop_all"),
		"cap_drop: [ALL] must pass: {findings:#?}"
	);
}

// ---------------------------------------------------------------------------
// no_new_privileges_off
// ---------------------------------------------------------------------------

#[test]
fn audit_no_new_privileges_off_flags_when_missing() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
"#;
	let findings = report_for(yaml);
	assert!(
		findings.iter().any(|f| f.check == "no_new_privileges_off"),
		"absent security_opt must fire: {findings:#?}"
	);
}
