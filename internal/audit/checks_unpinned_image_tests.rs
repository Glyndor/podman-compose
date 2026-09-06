//! Tests for the `unpinned_image` check, split from `checks_more_tests.rs`
//! to keep that file under the repository line limit. Two shapes to verify:
//! the basic tag verdict and the build/pull_policy matrix.

use super::tests::report_for;

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
