//! The second half of the per-check matrix, split from `checks_tests.rs` to
//! keep both files under the repository line limit. Same shape: one test
//! for the bad shape and one for the good shape of each check.

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

// ---------------------------------------------------------------------------
// secret_in_environment
// ---------------------------------------------------------------------------

#[test]
fn audit_secret_in_environment_flags_literal_secret_keys() {
	// One per keyword so a regression that misses one is caught by its own
	// test rather than masked by the others passing.
	for (key, _hint) in [
		("DB_PASSWORD", "PASSWORD"),
		("AUTH_SECRET", "SECRET"),
		("API_TOKEN", "TOKEN"),
		("SIGNING_KEY", "KEY"),
	] {
		let yaml = format!(
			"services:\n  app:\n    image: alpine:3.20\n    environment:\n      - {key}=literal\n"
		);
		let findings = report_for(&yaml);
		assert!(
			findings.iter().any(|f| f.check == "secret_in_environment"),
			"environment: {key}=literal must fire secret_in_environment; got {findings:#?}"
		);
		// The reason must name the key without echoing the value back ,
		// the literal value would defeat the whole point of the audit.
		let f = findings
			.iter()
			.find(|f| f.check == "secret_in_environment")
			.expect("finding");
		assert!(
			!f.reason.contains("literal"),
			"reason must not echo the value: {f:?}"
		);
		assert!(
			f.reason.contains(key),
			"reason must name the key {key}: {f:?}"
		);
	}
}

#[test]
fn audit_secret_in_environment_passes_for_unrelated_keys() {
	let yaml = r#"
services:
  app:
    image: alpine:3.20
    environment:
      - LOG_LEVEL=info
      - HOSTNAME=host-1
      - PORT=8080
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "secret_in_environment"),
		"unrelated env keys must not fire secret_in_environment: {findings:#?}"
	);
}

// ---------------------------------------------------------------------------
// unpinned_image
// ---------------------------------------------------------------------------

#[test]
fn audit_unpinned_image_flags_when_no_tag() {
	let yaml = r#"
services:
  web:
    image: nginx
"#;
	let findings = report_for(yaml);
	assert!(
		findings.iter().any(|f| f.check == "unpinned_image"),
		"untagged image must fire: {findings:#?}"
	);
}

#[test]
fn audit_unpinned_image_flags_when_tag_is_latest() {
	let yaml = r#"
services:
  web:
    image: nginx:latest
"#;
	let findings = report_for(yaml);
	assert!(
		findings.iter().any(|f| f.check == "unpinned_image"),
		"`:latest` must fire: {findings:#?}"
	);
}

#[test]
fn audit_unpinned_image_passes_when_tagged_not_latest() {
	// A non-default tag is the canonical "pinned to a version" case the
	// check is supposed to recognise. The tag value is irrelevant beyond
	// "is it the literal `latest`".
	let yaml = r#"
services:
  web:
    image: nginx:1.27.3
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "unpinned_image"),
		"explicit non-latest tag must pass: {findings:#?}"
	);
}

#[test]
fn audit_unpinned_image_ignores_services_without_image() {
	// A `build:` service has no registry reference: there is no tag to
	// pin. Out of scope for this check; the report must be empty (or
	// carry only unrelated findings).
	let yaml = r#"
services:
  web:
    build: .
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "unpinned_image"),
		"build-only services are out of scope for unpinned_image: {findings:#?}"
	);
}

#[test]
fn audit_unpinned_image_keeps_existing_message_when_no_build() {
	// Row 1 of the build+image+pull_policy matrix: a `:latest` image on a
	// service with no `build:` keeps the legacy wording because no locally
	// produced artifact is in play.
	let yaml = r#"
services:
  web:
    image: myapp:latest
"#;
	let findings = report_for(yaml);
	let f = findings
		.iter()
		.find(|f| f.check == "unpinned_image")
		.expect("unpinned_image must fire when no build is present");
	assert!(
		f.reason.contains("pins to :latest, which moves under you"),
		"row 1 must keep the legacy wording, got: {f:?}"
	);
	assert!(
		!f.reason.contains("is built here"),
		"row 1 must not switch to the built-here wording, got: {f:?}"
	);
}

#[test]
fn audit_unpinned_image_flags_build_service_with_latest_under_default_policy() {
	// Row 3 of the matrix: `build:` + `:latest` with the default
	// `pull_policy` (missing) still fires because the policy does not
	// forbid a fetch when the image is absent locally, so the
	// operator-actionable message has to name the policy as the fix.
	let yaml = r#"
services:
  web:
    image: myapp:latest
    build: .
"#;
	let findings = report_for(yaml);
	let f = findings
		.iter()
		.find(|f| f.check == "unpinned_image")
		.expect("unpinned_image must fire on a built service under default policy");
	assert!(
		f.reason.contains("is built here"),
		"row 3 must name the locally-built origin, got: {f:?}"
	);
	assert!(
		f.reason.contains("pull_policy: build"),
		"row 3 must recommend `pull_policy: build`, got: {f:?}"
	);
	assert!(
		!f.reason.contains("pins to :latest, which moves under you"),
		"row 3 must not use the legacy wording, got: {f:?}"
	);
}

#[test]
fn audit_unpinned_image_passes_when_build_and_pull_policy_build() {
	// Row 4 of the matrix: the policy itself commits to a local-only run,
	// so the check has nothing actionable to add.
	let yaml = r#"
services:
  web:
    image: myapp:latest
    build: .
    pull_policy: build
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "unpinned_image"),
		"`pull_policy: build` must suppress the check on a built service: {findings:#?}"
	);
}

#[test]
fn audit_unpinned_image_passes_when_build_and_pull_policy_never() {
	// Row 5 of the matrix: `pull_policy: never` is the other policy that
	// forbids the fetch, so the same skip applies.
	let yaml = r#"
services:
  web:
    image: myapp:latest
    build: .
    pull_policy: never
"#;
	let findings = report_for(yaml);
	assert!(
		!findings.iter().any(|f| f.check == "unpinned_image"),
		"`pull_policy: never` must suppress the check on a built service: {findings:#?}"
	);
}

#[test]
fn audit_unpinned_image_flags_build_service_with_pull_policy_always() {
	// Row 6 of the matrix: `pull_policy: always` still pulls even when
	// the image is local, so the registry can race the build; the
	// built-here message is the only one that names the right fix.
	let yaml = r#"
services:
  web:
    image: myapp:latest
    build: .
    pull_policy: always
"#;
	let findings = report_for(yaml);
	let f = findings
		.iter()
		.find(|f| f.check == "unpinned_image")
		.expect("unpinned_image must fire on a built service under pull_policy: always");
	assert!(
		f.reason.contains("is built here"),
		"row 6 must name the locally-built origin, got: {f:?}"
	);
	assert!(
		f.reason.contains("pull_policy: build"),
		"row 6 must recommend `pull_policy: build`, got: {f:?}"
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
