//! Pure helpers for build image tagging and context classification.
//!
//! These free functions back [`super::Engine::build_service`]: deciding whether
//! a build context is remote, choosing the primary image tag, and flagging
//! build-arg names that look like secrets. They hold no engine state, so they
//! live here apart from the build orchestration.

/// A build context is remote when it is a URL or Git reference that Podman
/// clones server-side, rather than a local directory to tar and upload.
pub(super) fn is_remote_context(context: &str) -> bool {
	context.contains("://") || context.starts_with("git@")
}

/// Pick the primary image tag for a built service.
///
/// Precedence matches compose-go: an explicit `image:` wins; otherwise the
/// first entry of `build.tags` is used as the primary tag; with neither, the
/// image is named `{project}-{service}:latest` (project-scoped, like docker
/// compose) so two projects sharing a service name don't overwrite each other's
/// image. Any remaining `build.tags` are applied as extra tags by
/// [`super::Engine::apply_extra_tags`].
pub(crate) fn primary_build_tag(
	project: &str,
	service_name: &str,
	image: Option<&str>,
	tags: &[String],
) -> String {
	if let Some(image) = image {
		return image.to_string();
	}
	if let Some(first) = tags.first() {
		return first.clone();
	}
	format!("{project}-{service_name}:latest")
}

/// True if a build-arg name looks like it carries a secret, so the caller can
/// warn that build args persist in the image history. Case-insensitive substring
/// match on common secret tokens, kept conservative to avoid false positives
/// (e.g. a bare `KEY` is not flagged; `API_KEY`/`PRIVATE_KEY` are).
pub(super) fn looks_like_secret(name: &str) -> bool {
	const MARKERS: [&str; 7] = [
		"SECRET",
		"PASSWORD",
		"PASSWD",
		"TOKEN",
		"CREDENTIAL",
		"API_KEY",
		"PRIVATE_KEY",
	];
	let upper = name.to_ascii_uppercase();
	MARKERS.iter().any(|m| upper.contains(m))
}

#[cfg(test)]
#[path = "tags_tests.rs"]
mod tests;
