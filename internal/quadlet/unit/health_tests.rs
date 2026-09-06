use super::{render_healthcheck, Section};
use crate::compose::types::Service;

/// Render a `[Container]` section for a service parsed from `yaml`,
/// returning `(rendered_body, warnings)`.
fn render(yaml: &str) -> (String, Vec<String>) {
	let service: Service = serde_yaml::from_str(yaml).expect("service parses");
	let mut container = Section::new("Container");
	let mut warnings = Vec::new();
	render_healthcheck("web", &service, &mut container, &mut warnings);
	(container.render(), warnings)
}

#[test]
fn no_healthcheck_emits_nothing() {
	let (body, warnings) = render("image: x\n");
	assert_eq!(body, "[Container]\n");
	assert!(warnings.is_empty());
}

#[test]
fn disabled_healthcheck_emits_none() {
	let (body, warnings) = render("image: x\nhealthcheck:\n  disable: true\n");
	assert!(body.contains("HealthCmd=none"));
	// A disabled check short-circuits, so no timing keys follow.
	assert!(!body.contains("HealthInterval"));
	assert!(warnings.is_empty());
}

#[test]
fn exec_test_strips_cmd_sentinel_and_renders_timing() {
	let (body, warnings) = render(
		"image: x\nhealthcheck:\n  test: [\"CMD\", \"curl\", \"-f\", \"http://localhost\"]\n  \
		 interval: 30s\n  timeout: 5s\n  retries: 3\n  start_period: 10s\n",
	);
	// The CMD sentinel is dropped from the rendered command.
	assert!(body.contains("HealthCmd=curl -f http://localhost"));
	assert!(body.contains("HealthInterval=30s"));
	assert!(body.contains("HealthTimeout=5s"));
	assert!(body.contains("HealthRetries=3"));
	assert!(body.contains("HealthStartPeriod=10s"));
	assert!(warnings.is_empty());
}

#[test]
fn shell_test_renders_as_is() {
	let (body, _) = render("image: x\nhealthcheck:\n  test: curl -f http://localhost\n");
	assert!(body.contains("HealthCmd=curl -f http://localhost"));
}

#[test]
fn exec_test_without_sentinel_keeps_all_parts() {
	// An exec array whose first element is not CMD/CMD-SHELL/NONE is rendered
	// verbatim (no sentinel stripped).
	let (body, _) = render("image: x\nhealthcheck:\n  test: [\"pg_isready\", \"-q\"]\n");
	assert!(body.contains("HealthCmd=pg_isready -q"));
}

#[test]
fn start_interval_is_skipped_with_warning() {
	let (body, warnings) =
		render("image: x\nhealthcheck:\n  test: [\"CMD\", \"true\"]\n  start_interval: 5s\n");
	assert!(body.contains("HealthCmd=true"));
	// No key is emitted for start_interval, but the user is warned.
	assert_eq!(warnings.len(), 1);
	assert!(warnings[0].contains("start_interval"));
}

/// Without this systemd calls the unit started as soon as the container is
/// created, so `After=`/`Requires=` order creation rather than readiness.
/// Measured on podman 5.7.0: a dependant started 10s into a dependency
/// whose probe took 10s, instead of immediately.
#[test]
fn a_healthcheck_brings_notify_healthy() {
	let (body, _) = render("image: x\nhealthcheck:\n  test: [\"CMD\", \"true\"]\n");
	assert!(body.contains("Notify=healthy"), "{body}");
}

/// `--sdnotify=healthy` with nothing to probe would leave the unit starting
/// until `TimeoutStartSec` expires, so it must never be emitted alone.
#[test]
fn no_healthcheck_means_no_notify() {
	let (body, _) = render("image: x\n");
	assert!(!body.contains("Notify"), "{body}");
}

#[test]
fn a_disabled_healthcheck_means_no_notify() {
	let (body, _) = render("image: x\nhealthcheck:\n  disable: true\n");
	assert!(!body.contains("Notify"), "{body}");
}

/// An empty command emits no `HealthCmd`, so there is nothing for systemd
/// to wait on and `Notify` must not appear either.
#[test]
fn an_empty_test_means_no_notify() {
	let (body, _) = render("image: x\nhealthcheck:\n  test: []\n");
	assert!(!body.contains("Notify"), "{body}");
}
