use super::{quadlet_platform_advisory, write_quadlet};
use podup::parse_str;
use podup::quadlet::validate_for_quadlet;

#[test]
fn quadlet_advisory_only_on_non_linux() {
	assert_eq!(quadlet_platform_advisory("linux"), None);
	for os in ["macos", "windows", "freebsd"] {
		let msg = quadlet_platform_advisory(os).expect("non-linux host warns");
		assert!(msg.contains("systemd"), "advisory names the requirement");
	}
}

#[test]
fn cyclic_depends_on_is_rejected_before_emitting_units() {
	// A `depends_on` cycle must error rather than emit units with mutual
	// `After=`/`Requires=`; the check runs before any file I/O so `output: None`
	// is safe here.
	let file = podup::parse_str(
		"services:\n  a:\n    image: x\n    depends_on: [b]\n  b:\n    image: y\n    depends_on: [a]\n",
	)
	.unwrap();
	let err =
		write_quadlet(&file, "proj", std::path::Path::new("/srv/app"), None, false).unwrap_err();
	assert!(matches!(err, podup::ComposeError::CircularDependency(_)));
}

#[test]
fn valid_compose_passes_validation() {
	let file = parse_str("services:\n  web:\n    image: nginx\n").unwrap();
	assert!(validate_for_quadlet(&file).is_ok());
}

#[test]
fn service_without_image_or_build_is_rejected() {
	// `generate quadlet` must reject the same config `config`/`up` reject
	// rather than emit a `[Container]` with no `Image=`.
	let file = parse_str("services:\n  web:\n    ports:\n      - \"8080:80\"\n").unwrap();
	let err = validate_for_quadlet(&file).unwrap_err();
	assert!(matches!(err, podup::ComposeError::NoImageOrBuild(_)));
}

#[test]
fn out_of_range_port_is_rejected() {
	// A port above u16 must error, not be re-emitted as an invalid PublishPort.
	let file =
		parse_str("services:\n  web:\n    image: x\n    ports:\n      - \"70000:80\"\n").unwrap();
	assert!(validate_for_quadlet(&file).is_err());
}

#[test]
fn malformed_mem_limit_is_rejected() {
	let file = parse_str("services:\n  web:\n    image: x\n    mem_limit: abc\n").unwrap();
	let err = validate_for_quadlet(&file).unwrap_err();
	assert!(matches!(err, podup::ComposeError::Unsupported(_)));
}

#[test]
fn dependency_cycle_is_rejected() {
	// A `depends_on` cycle would emit a systemd ordering cycle; reject it like
	// `up`/`create` do.
	let yaml = "services:\n  a:\n    image: x\n    depends_on: [b]\n  b:\n    image: x\n    depends_on: [a]\n";
	let file = parse_str(yaml).unwrap();
	let err = validate_for_quadlet(&file).unwrap_err();
	assert!(matches!(err, podup::ComposeError::CircularDependency(_)));
}

#[test]
fn missing_dependency_is_not_fatal() {
	// An `After=` referencing an externally-managed unit is allowed; only
	// cycles are rejected.
	let file = parse_str("services:\n  web:\n    image: x\n    depends_on: [db]\n").unwrap();
	assert!(validate_for_quadlet(&file).is_ok());
}
