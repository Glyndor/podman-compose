//! Tests for start mode's renderer and its refusals.

use super::start::*;
use crate::compose::parse_str;
use std::path::PathBuf;

fn opts() -> StartUnitOpts {
	StartUnitOpts::new(
		PathBuf::from("/usr/bin/podman"),
		"app".to_string(),
		"app-web-1".to_string(),
	)
}

#[test]
fn the_boot_path_carries_no_compose_file() {
	// The whole mode. `podman start` restores the container definition from
	// Podman's store, so nothing here may reach for the file, the environment,
	// a registry or a build. Asserted as the positive fact first, then as the
	// absences, so a unit that rendered nothing at all cannot pass.
	let s = render_start_unit(&opts());
	assert!(
		s.contains("ExecStart=/usr/bin/podman start app-web-1"),
		"{s}"
	);
	assert!(s.contains("ExecStop=/usr/bin/podman stop app-web-1"), "{s}");
	// Scoped to the exec lines: `Description=podup <project>` legitimately
	// names podup, and asserting over the whole unit would fail on the label
	// rather than on the boot path, which is the thing under test.
	let execs: String = s
		.lines()
		.filter(|l| l.starts_with("ExecStart=") || l.starts_with("ExecStop="))
		.collect::<Vec<_>>()
		.join("\n");
	assert_eq!(execs.lines().count(), 2, "{s}");
	for absent in ["podup", "-f ", "--env-file", "up -d", "--build"] {
		assert!(
			!execs.contains(absent),
			"the boot path must not carry `{absent}`:\n{execs}"
		);
	}
}

#[test]
fn execstop_stops_rather_than_removes() {
	// `rm` would delete the container the next boot's ExecStart expects to
	// find, turning every reboot into a broken start. Same contract service
	// mode pins with `down`.
	let s = render_start_unit(&opts());
	assert!(s.contains(" stop app-web-1"), "{s}");
	assert!(!s.contains(" rm "), "ExecStop must not remove:\n{s}");
}

#[test]
fn orders_against_the_user_scope_network_shim() {
	let s = render_start_unit(&opts());
	assert!(
		s.contains("Wants=podman-user-wait-network-online.service"),
		"{s}"
	);
	assert!(
		s.contains("After=podman-user-wait-network-online.service"),
		"{s}"
	);
	for key in ["Wants=", "After=", "Requires=", "BindsTo=", "PartOf="] {
		assert!(
			!s.contains(&format!("{key}network-online.target")),
			"a `--user` unit must not depend on the system target directly:\n{s}"
		);
	}
}

#[test]
fn stop_timeout_leaves_headroom_and_is_omitted_when_unset() {
	let s = render_start_unit(&opts().with_stop_grace_secs(Some(10)));
	assert!(s.contains("TimeoutStopSec=40"), "{s}");
	let s = render_start_unit(&opts());
	assert!(!s.contains("TimeoutStopSec"), "{s}");
}

#[test]
fn percent_is_doubled_in_every_interpolated_value() {
	let mut o = opts();
	o.container = "50%off".to_string();
	o.project = "50%h".to_string();
	let s = render_start_unit(&o);
	assert!(s.contains("Description=podup 50%%h"), "{s}");
	assert!(s.contains("\"50%%off\""), "{s}");
	assert!(!s.contains("start 50%off"), "{s}");
}

#[test]
fn a_path_with_spaces_stays_one_argument() {
	let mut o = opts();
	o.podman = PathBuf::from("/opt/my tools/podman");
	let s = render_start_unit(&o);
	assert!(
		s.contains("ExecStart=\"/opt/my tools/podman\" start"),
		"{s}"
	);
}

#[test]
fn validate_rejects_control_characters() {
	let mut o = opts();
	o.container = "web\nExecStartPre=/bin/evil".to_string();
	assert!(validate_start_unit_opts(&o).is_err());
	assert!(validate_start_unit_opts(&opts()).is_ok());
}

#[test]
fn one_service_resolves_to_the_engine_name() {
	let f = parse_str("services:\n  web:\n    image: x\n").unwrap();
	assert_eq!(sole_container(&f, "app").unwrap(), "app-web-1");
}

#[test]
fn an_explicit_container_name_is_honoured() {
	let f =
		parse_str("services:\n  web:\n    image: x\n    container_name: alanalarana\n").unwrap();
	assert_eq!(sole_container(&f, "app").unwrap(), "alanalarana");
}

#[test]
fn two_services_are_refused_and_the_message_names_quadlet() {
	let f = parse_str("services:\n  web:\n    image: x\n  db:\n    image: y\n").unwrap();
	let why = sole_container(&f, "app").unwrap_err();
	assert!(
		matches!(&why, StartModeRefusal::MultipleServices(n) if n.len() == 2),
		"{why:?}"
	);
	// The refusal has to say where to go, or it is a dead end rather than a
	// boundary.
	let msg = why.to_string();
	assert!(msg.contains("--mode quadlet"), "{msg}");
	assert!(msg.contains("depends_on"), "{msg}");
}

#[test]
fn a_scaled_service_is_refused() {
	let f = parse_str("services:\n  web:\n    image: x\n    scale: 3\n").unwrap();
	assert_eq!(
		sole_container(&f, "app").unwrap_err(),
		StartModeRefusal::MultipleReplicas {
			service: "web".to_string(),
			replicas: 3,
		}
	);
}

#[test]
fn deploy_replicas_counts_the_same_as_scale() {
	let f = parse_str("services:\n  web:\n    image: x\n    deploy:\n      replicas: 2\n").unwrap();
	assert!(matches!(
		sole_container(&f, "app").unwrap_err(),
		StartModeRefusal::MultipleReplicas { replicas: 2, .. }
	));
}

#[test]
fn a_file_with_no_services_is_refused() {
	let f = parse_str("services: {}\n").unwrap();
	assert_eq!(
		sole_container(&f, "app").unwrap_err(),
		StartModeRefusal::NoServices
	);
}
