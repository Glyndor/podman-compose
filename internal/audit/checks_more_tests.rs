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

// ---------------------------------------------------------------------------
// #1743: audit must read the resolved value the engine will use
// ---------------------------------------------------------------------------
//
// Each of the three tests below targets one of the issue's findings and is
// built from a compose file that `audit --strict` accepted on the unfixed
// tree but must not afterwards. They are the issue's acceptance criterion,
// and were failing when the brief was written. Keeping them in a single
// section so the file's `grep "1743"` lands the reviewer on the lot.

/// `mem_limit: not-a-size` keeps the compose field non-empty, so an
/// `.is_none()` audit reads it as a limit, but `parse_memory` returns
/// `None` and the runtime applies no limit. The audit must agree.
#[test]
fn audit_no_memory_limit_flags_an_unparseable_limit() {
	let report =
		report_for("services:\n  web:\n    image: alpine:3.20\n    mem_limit: not-a-size\n");
	assert!(
		report.iter().any(|f| f.check == "no_memory_limit"),
		"a mem_limit the runtime cannot parse must still fire: {report:#?}"
	);
}

/// `security_opt: [no-new-privileges:true, no-new-privileges:false]`: the
/// engine last-wins (resolves to disabled), the unfixed audit iterates and
/// finds the first entry matches (resolves to enabled). The two must agree.
#[test]
fn audit_no_new_privileges_off_flags_contradictory_entries() {
	let report = report_for(
		"services:\n  web:\n    image: alpine:3.20\n    security_opt:\n      - no-new-privileges:true\n      - no-new-privileges:false\n",
	);
	assert!(
		report.iter().any(|f| f.check == "no_new_privileges_off"),
		"contradictory security_opt entries where the engine disables the \
		 protection must fire: {report:#?}"
	);
}

/// The runtime honours exactly what was asked for in `cap_add:`. The audit
/// must reject the curated dangerous list (`SYS_MODULE` and friends)
/// regardless of what the engine does with them.
#[test]
fn audit_dangerous_capability_flags_the_curated_list() {
	let report = report_for(
		"services:\n  web:\n    image: alpine:3.20\n    cap_add: [SYS_MODULE, DAC_READ_SEARCH, SYS_RAWIO]\n",
	);
	assert!(
		report.iter().any(|f| f.check == "dangerous_capability"),
		"SYS_MODULE / DAC_READ_SEARCH / SYS_RAWIO must fire: {report:#?}"
	);
	// Every entry in the curated list is its own finding, so the count
	// pins the list rather than just any-of.
	assert_eq!(
		report
			.iter()
			.filter(|f| f.check == "dangerous_capability")
			.count(),
		3,
		"three distinct dangerous_capability findings expected: {report:#?}"
	);
}
