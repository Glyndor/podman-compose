use super::{is_remote_context, looks_like_secret, primary_build_tag};

#[test]
fn looks_like_secret_flags_sensitive_names_only() {
	for name in [
		"DB_PASSWORD",
		"api_token",
		"MySecret",
		"AWS_API_KEY",
		"private_key",
	] {
		assert!(looks_like_secret(name), "{name} should be flagged");
	}
	for name in ["VERSION", "BUILD_DATE", "PUBLIC_KEY", "PORT", "RUST_LOG"] {
		assert!(!looks_like_secret(name), "{name} should not be flagged");
	}
}

#[test]
fn remote_context_detection() {
	assert!(is_remote_context("https://github.com/user/repo.git"));
	assert!(is_remote_context("git://example.com/repo.git"));
	assert!(is_remote_context("git@github.com:user/repo.git"));
	assert!(!is_remote_context("."));
	assert!(!is_remote_context("./build"));
	assert!(!is_remote_context("/abs/path"));
}

#[test]
fn primary_tag_prefers_explicit_image() {
	let tags = vec!["registry/app:1.0".to_string()];
	assert_eq!(
		primary_build_tag("proj", "app", Some("myimage:2.0"), &tags),
		"myimage:2.0"
	);
}

#[test]
fn primary_tag_uses_first_build_tag_when_image_unset() {
	let tags = vec![
		"registry/app:1.0".to_string(),
		"registry/app:latest".to_string(),
	];
	assert_eq!(
		primary_build_tag("proj", "app", None, &tags),
		"registry/app:1.0"
	);
}

#[test]
fn primary_tag_falls_back_to_project_scoped_latest() {
	// Build-only services (no `image:`, no `tags`) are namespaced by project so
	// two projects sharing a service name don't clobber each other's image.
	assert_eq!(
		primary_build_tag("proj", "app", None, &[]),
		"proj-app:latest"
	);
	assert_eq!(
		primary_build_tag("other", "app", None, &[]),
		"other-app:latest"
	);
}
