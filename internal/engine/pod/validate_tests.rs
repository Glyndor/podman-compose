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

/// Two services on the same declared set are fine; only a differing set is
/// refused. A sweep that refused every explicit set stayed green without this.
#[test]
fn pod_accepts_two_services_on_the_same_declared_networks() {
	let yaml = r#"
services:
  web:
    image: nginx
    networks: [backend, front]
  db:
    image: postgres
    networks: [front, backend]
networks:
  backend:
  front:
"#;
	check(yaml).expect("identical network sets must be accepted");
}

/// Host port 0 asks Podman for an ephemeral port; two services asking for
/// one each do not collide.
#[test]
fn pod_accepts_two_ephemeral_host_ports() {
	let yaml = r#"
services:
  web:
    image: nginx
    ports: ["0:80"]
  api:
    image: nginx
    ports: ["0:8080"]
"#;
	check(yaml).expect("two ephemeral host ports must be accepted");
}

/// The duplicate-port rule does not care which host IP each service binds:
/// the pod publishes the union, and one host port there belongs to one
/// service.
#[test]
fn pod_refuses_the_same_host_port_across_services_even_on_different_ips() {
	let yaml = r#"
services:
  web:
    image: nginx
    ports: ["127.0.0.1:8080:80"]
  api:
    image: nginx
    ports: ["10.0.0.1:8080:8080"]
"#;
	let err = check(yaml).expect_err("one host port on two IPs must be refused");
	assert!(
		err.contains("8080") && err.contains("web") && err.contains("api"),
		"the message must name the port and both services: {err}"
	);
}

/// One service may bind the same host port twice for two protocols; the
/// rule is per service, port and protocol.
#[test]
fn pod_accepts_the_same_host_port_and_ip_for_two_protocols() {
	let yaml = r#"
services:
  dns:
    image: coredns
    ports: ["127.0.0.1:5353:53/tcp", "127.0.0.1:5353:53/udp"]
"#;
	check(yaml).expect("the same port on the same IP for two protocols must be accepted");
}

/// One user namespace per pod: services that disagree on `userns_mode` are
/// refused, naming both, and the unset case counts as a value.
#[test]
fn pod_refuses_services_that_disagree_on_userns_mode() {
	let yaml = r#"
services:
  web:
    image: nginx
    userns_mode: auto
  db:
    image: postgres
"#;
	let err = check(yaml).expect_err("a userns_mode on one service only must be refused");
	assert!(
		err.contains("userns_mode") && err.contains("web") && err.contains("db"),
		"{err}"
	);
}

#[test]
fn pod_accepts_services_that_agree_on_userns_mode() {
	let yaml = r#"
services:
  web:
    image: nginx
    userns_mode: auto
  db:
    image: postgres
    userns_mode: auto
"#;
	check(yaml).expect("the same userns_mode on every service must be accepted");
}
