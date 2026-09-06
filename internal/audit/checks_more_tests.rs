//! The second half of the per-check matrix, split from `checks_tests.rs` to
//! keep both files under the repository line limit. Holds the
//! positive/negative pairs for the resource-limit and userns checks, plus
//! the "every spelling" extensions for `dangerous_capability`,
//! `no_cap_drop_all`, and `no_new_privileges_off`. The two checks with the
//! densest coverage (`secret_in_environment`, `unpinned_image`) live in
//! their own files.

use super::tests::report_for;

#[test]
fn audit_no_new_privileges_off_passes_in_both_spellings() {
	// Podman spells it `no-new-privileges` (no `:true`); the docker form
	// carries `:true`. Both must count.
	for entry in ["no-new-privileges", "no-new-privileges:true"] {
		let yaml = format!(
			"services:\n  web:\n    image: alpine:3.20\n    security_opt:\n      - {entry}\n"
		);
		let findings = report_for(&yaml);
		assert!(
			!findings.iter().any(|f| f.check == "no_new_privileges_off"),
			"`{entry}` must pass the no-new-privileges-off check: {findings:#?}"
		);
	}
}

// ---------------------------------------------------------------------------
// no_pids_limit
// ---------------------------------------------------------------------------

#[test]
fn audit_no_pids_limit_flags_when_unset() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
"#;
	let findings = report_for(yaml);
	assert!(
		findings.iter().any(|f| f.check == "no_pids_limit"),
		"unset pids_limit must fire: {findings:#?}"
	);
}

#[test]
fn audit_no_pids_limit_passes_when_set() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    pids_limit: 200
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "no_pids_limit"),
		"pids_limit: 200 must pass: {findings:#?}"
	);
}

// ---------------------------------------------------------------------------
// no_memory_limit
// ---------------------------------------------------------------------------

#[test]
fn audit_no_memory_limit_flags_when_unset() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
"#;
	let findings = report_for(yaml);
	assert!(
		findings.iter().any(|f| f.check == "no_memory_limit"),
		"unset memory limit must fire: {findings:#?}"
	);
}

#[test]
fn audit_no_memory_limit_passes_when_set_on_mem_limit() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    mem_limit: 512m
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "no_memory_limit"),
		"mem_limit must pass: {findings:#?}"
	);
}

#[test]
fn audit_no_memory_limit_passes_when_set_on_deploy_resources() {
	// The check accepts either location; `deploy.resources.limits.memory`
	// covers the swarm-style shape that some compose files adopted as
	// rootless Podman grew up.
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    deploy:
      resources:
        limits:
          memory: 256M
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "no_memory_limit"),
		"deploy.resources.limits.memory must pass: {findings:#?}"
	);
}

// ---------------------------------------------------------------------------
// no_userns
// ---------------------------------------------------------------------------

#[test]
fn audit_no_userns_flags_when_unset() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
"#;
	let findings = report_for(yaml);
	assert!(
		findings.iter().any(|f| f.check == "no_userns"),
		"unset userns_mode must fire: {findings:#?}"
	);
}

#[test]
fn audit_no_userns_passes_when_set() {
	let yaml = r#"
services:
  web:
    image: alpine:3.20
    userns_mode: keep-id
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "no_userns"),
		"userns_mode: keep-id must pass: {findings:#?}"
	);
}

/// `CAP_SYS_ADMIN`, `sys_admin` and `SYS_ADMIN` are the same capability, and
/// compose files carry all three spellings; the check has to read them alike.
#[test]
fn audit_dangerous_capability_reads_every_spelling() {
	for spelling in ["CAP_SYS_ADMIN", "sys_admin", "cap_all", "ALL"] {
		let report = report_for(&format!(
			"services:\n  web:\n    image: alpine:3.20\n    cap_add: [{spelling}]\n"
		));
		assert!(
			report.iter().any(|f| f.check == "dangerous_capability"),
			"{spelling} was not read as dangerous"
		);
	}
}

/// `cap_drop: [all]` and `cap_drop: [CAP_ALL]` satisfy the check like `ALL`.
#[test]
fn audit_no_cap_drop_all_accepts_every_spelling() {
	for spelling in ["all", "CAP_ALL", "ALL"] {
		let report = report_for(&format!(
			"services:\n  web:\n    image: alpine:3.20\n    cap_drop: [{spelling}]\n"
		));
		assert!(
			!report.iter().any(|f| f.check == "no_cap_drop_all"),
			"{spelling} was not read as ALL"
		);
	}
}

/// `no-new-privileges:false` is the option switched off, not on.
#[test]
fn audit_no_new_privileges_off_flags_an_explicit_false() {
	let report = report_for(
		"services:\n  web:\n    image: alpine:3.20\n    security_opt: [\"no-new-privileges:false\"]\n",
	);
	assert!(report.iter().any(|f| f.check == "no_new_privileges_off"));
}
