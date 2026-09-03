//! Unit tests for the `x-podman-pod` pre-flight refusals.
//!
//! Each refusal has its own one-test lock so a failing assertion names the
//! case it broke. The tests live next to the validator rather than in a
//! shared `tests.rs` because every test pins one shape of the compose
//! file and the file's full text is the most readable way to read each
//! case.

use crate::compose::parse_str;
use crate::engine::pod::validate_pod_or_refuse;

fn check(yaml: &str) -> Result<(), String> {
	let file = parse_str(yaml).expect("compose file must parse");
	validate_pod_or_refuse(&file)
}

/// A service declaring `network_mode: host` is incompatible with the pod's
/// shared namespace and is refused with a message naming the service and
/// the offending key.
#[test]
fn pod_refuses_network_mode() {
	let yaml = r#"
services:
  web:
    image: nginx
    network_mode: host
"#;
	let err = check(yaml).expect_err("network_mode must be refused");
	assert!(
		err.contains("web") && err.contains("network_mode"),
		"expected service name and key in the message: {err}"
	);
}

/// Two services with different `networks:` sets must agree or one of them
/// is refused. The check is order-sensitive on the first non-empty set
/// declared, so a leading service with a single network wins.
#[test]
fn pod_refuses_divergent_networks() {
	let yaml = r#"
services:
  web:
    image: nginx
    networks: [frontend]
  db:
    image: postgres
    networks: [backend]
"#;
	let err = check(yaml).expect_err("divergent networks must be refused");
	assert!(
		err.contains("db") && err.contains("networks"),
		"expected service name and key in the message: {err}"
	);
}

/// Two services publishing the same host port would hand Podman duplicate
/// port mappings. Refuse up front so the user fixes the compose file
/// rather than chasing a libpod 500.
#[test]
fn pod_refuses_duplicate_host_port() {
	let yaml = r#"
services:
  web:
    image: nginx
    ports: ["8080:80"]
  api:
    image: nginx
    ports: ["8080:80"]
"#;
	let err = check(yaml).expect_err("duplicate host port must be refused");
	assert!(
		err.contains("8080") && (err.contains("web") || err.contains("api")),
		"expected port and service names in the message: {err}"
	);
}

/// A plain two-service project with `x-podman-pod: true` and no network
/// collisions must pass validation.
#[test]
fn pod_accepts_a_plain_two_service_project() {
	let yaml = r#"
x-podman-pod: true
services:
  web:
    image: nginx
    ports: ["8080:80"]
  db:
    image: postgres
    environment:
      POSTGRES_PASSWORD: secret
"#;
	check(yaml).expect("a plain two-service project must pass");
}
