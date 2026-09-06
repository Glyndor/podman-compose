use super::ComposeError;

#[test]
fn display_covers_all_variants() {
	let cases: &[(&str, ComposeError)] = &[
		(
			"failed to parse compose file",
			ComposeError::Parse(serde_yaml::from_str::<serde_yaml::Value>(":\0").unwrap_err()),
		),
		(
			"compose file not found: f",
			ComposeError::FileNotFound("f".into()),
		),
		("io error:", ComposeError::Io(std::io::Error::other("x"))),
		(
			"service 's' not found",
			ComposeError::ServiceNotFound("s".into()),
		),
		("c", ComposeError::CircularDependency("c".into())),
		(
			"service 'svc' has no image or build config",
			ComposeError::NoImageOrBuild("svc".into()),
		),
		(
			"required variable 'V' is not set: reason",
			ComposeError::RequiredVarNotSet {
				var: "V".into(),
				msg: "reason".into(),
			},
		),
		(
			"health check timeout for container 'c'",
			ComposeError::HealthCheckTimeout("c".into()),
		),
		(
			"invalid port mapping: p",
			ComposeError::InvalidPort("p".into()),
		),
		(
			"podman error:",
			ComposeError::Podman(crate::libpod::PodmanError::Api {
				status: 500,
				message: "boom".into(),
			}),
		),
		(
			"invalid variable substitution: bad",
			ComposeError::InvalidSubstitution("bad".into()),
		),
		("build error: b", ComposeError::Build("b".into())),
		("cp error: c", ComposeError::Copy("c".into())),
		("extends error: e", ComposeError::Extends("e".into())),
		("include error: i", ComposeError::Include("i".into())),
		("watch error: w", ComposeError::Watch("w".into())),
		(
			"unsupported feature: u",
			ComposeError::Unsupported("u".into()),
		),
		(
			"run container exited with code 1",
			ComposeError::RunExited(1),
		),
		("update error: u", ComposeError::Update("u".into())),
		(
			"external resource not found: external volume \"v\" does not exist",
			ComposeError::ExternalNotFound("external volume \"v\" does not exist".into()),
		),
		(
			"service 'web' publishes fixed host port(s) [8080] but is scaled to 3 replicas",
			ComposeError::ScalePortConflict {
				service: "web".into(),
				replicas: 3,
				ports: vec![8080],
			},
		),
		(
			"container 'web' exited with code 7 while waiting for it to be ready",
			ComposeError::WaitServiceExited {
				container: "web".into(),
				code: 7,
			},
		),
		(
			"service 'web' requests 100000 replicas, which exceeds the limit of 256",
			ComposeError::ReplicaLimitExceeded {
				service: "web".into(),
				replicas: 100_000,
				max: 256,
			},
		),
		(
			"timed out after 30s waiting for services to become healthy",
			ComposeError::WaitTimeout { secs: 30 },
		),
		(
			"service 'web' has no replica 99 (replica indexes are 1-based)",
			ComposeError::ReplicaIndex {
				service: "web".into(),
				index: 99,
			},
		),
		(
			"io error: /out/x.tar:",
			ComposeError::IoPath {
				path: "/out/x.tar".into(),
				source: std::io::Error::other("boom"),
			},
		),
		(
			"build context './ctx' for service 'web':",
			ComposeError::BuildContext {
				service: "web".into(),
				path: "./ctx".into(),
				source: std::io::Error::other("boom"),
			},
		),
		(
			"service 'web' is not running",
			ComposeError::NotRunning("web".into()),
		),
		(
			"exec failed: the exec session did not start within 20s",
			ComposeError::ExecFailed("the exec session did not start within 20s".into()),
		),
		(
			"invalid --timeout -5: use -1 to wait indefinitely or a non-negative number of seconds",
			ComposeError::InvalidTimeout(-5),
		),
		(
			"env file not found: app.env",
			ComposeError::EnvFile("env file not found: app.env".into()),
		),
		(
			"linger is not enabled",
			ComposeError::Autostart("linger is not enabled".into()),
		),
	];
	for (expected_prefix, err) in cases {
		let msg = err.to_string();
		assert!(
			msg.starts_with(expected_prefix),
			"Display for {:?}: got {msg:?}, expected prefix {expected_prefix:?}",
			std::mem::discriminant(err),
		);
	}
}

#[test]
fn parse_display_does_not_echo_offending_scalar() {
	// A type error embeds the offending scalar in the raw serde_yaml message
	// (`invalid type: string "s3cr3t-token", ...`). The Display must not surface
	// that content; it points at the location instead, so a non-compose file
	// pointed at with `-f` cannot leak its bytes onto stderr.
	#[derive(Debug, serde::Deserialize)]
	struct OnlyMap {
		#[allow(dead_code)]
		services: std::collections::BTreeMap<String, String>,
	}
	let err = serde_yaml::from_str::<OnlyMap>("services: s3cr3t-token\n").unwrap_err();
	let msg = ComposeError::Parse(err).to_string();
	assert!(
		!msg.contains("s3cr3t-token"),
		"parse error must not echo file content, got {msg:?}"
	);
	assert!(msg.starts_with("failed to parse compose file"));
}

#[test]
fn source_provided_for_wrapped_variants() {
	use std::error::Error;
	let io = ComposeError::Io(std::io::Error::other("x"));
	assert!(io.source().is_some());
	// Parse and Podman also wrap a lower-level error and expose it.
	let parse = ComposeError::Parse(serde_yaml::from_str::<serde_yaml::Value>(":\0").unwrap_err());
	assert!(parse.source().is_some());
	let podman = ComposeError::Podman(crate::libpod::PodmanError::Api {
		status: 500,
		message: "boom".into(),
	});
	assert!(podman.source().is_some());
	let svc = ComposeError::ServiceNotFound("s".into());
	assert!(svc.source().is_none());
}

#[test]
fn service_name_control_chars_are_escaped_in_display() {
	// A crafted name carrying an ESC sequence and newline must not reach the
	// terminal raw: the control bytes are escaped, the quotes preserved.
	let err = ComposeError::ServiceNotFound("we\x1b[31mb\n".into());
	let msg = err.to_string();
	assert!(!msg.contains('\x1b'), "ESC must be escaped: {msg:?}");
	assert!(!msg.contains('\n'), "newline must be escaped: {msg:?}");
	assert!(
		msg.contains("\\u{1b}") && msg.contains("\\n"),
		"got {msg:?}"
	);
}

#[test]
fn replica_index_hint_is_outside_the_quoted_name() {
	// The hint must render after the closing quote, not inside the service name.
	let err = ComposeError::ReplicaIndex {
		service: "web".into(),
		index: 0,
	};
	let msg = err.to_string();
	assert!(msg.contains("'web'"), "service name stays clean: {msg:?}");
	assert!(!msg.contains("'web "), "hint leaked into the name: {msg:?}");
	assert!(msg.contains("1-based"));
}

#[test]
fn from_impls_convert_correctly() {
	let io_err = std::io::Error::other("x");
	let e: ComposeError = io_err.into();
	assert!(matches!(e, ComposeError::Io(_)));

	let yaml_err = serde_yaml::from_str::<serde_yaml::Value>(":\0").unwrap_err();
	let e: ComposeError = yaml_err.into();
	assert!(matches!(e, ComposeError::Parse(_)));

	let podman_err = crate::libpod::PodmanError::Api {
		status: 404,
		message: "not found".into(),
	};
	let e: ComposeError = podman_err.into();
	assert!(matches!(e, ComposeError::Podman(_)));
}
