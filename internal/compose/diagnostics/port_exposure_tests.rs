//! Tests for the published-port exposure warning.
//!
//! Split out of `tests.rs`, which the port cases pushed over the per-file
//! line limit, the same way that file was split out of `mod.rs`.

use crate::parse_str;

fn diagnostics_for(yaml: &str) -> Vec<String> {
	let file = parse_str(yaml).unwrap();
	super::super::collect(&file)
}

/// `5432:5432` is the canonical accidental case: the operator writes it
/// thinking "this is for me", and the compose-spec default binds on every
/// host interface. The behavior is left as-is (docker-compose does the same)
/// but the warning makes the foot-gun visible.
#[test]
fn warns_on_short_port_without_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"5432:5432\"\n",
	);
	assert!(
		msgs.iter().any(|m| m.contains("service 'db'")
			&& m.contains("port 5432")
			&& m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// `127.0.0.1:5432:5432` is the explicit-loopback form: the bind is
/// restricted to the host, so nothing leaks to the network and no warning
/// is due.
#[test]
fn does_not_warn_on_short_port_with_loopback_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"127.0.0.1:5432:5432\"\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// `0.0.0.0:5432:5432` does the same bind as the IP-less short form, every
/// interface, but the operator typed it explicitly. Warning here would
/// train the reader to ignore this warning, so this is the case that looks
/// wrong but is deliberate.
#[test]
fn does_not_warn_on_short_port_with_explicit_all_interfaces_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"0.0.0.0:5432:5432\"\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// A service with no `ports:` field publishes nothing on the host, so the
/// warning has nothing to attach to.
#[test]
fn does_not_warn_when_service_has_no_ports() {
	let msgs = diagnostics_for("services:\n  web:\n    image: nginx\n");
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// A service with only `expose:` (no `ports:`) is reachable to peers on the
/// same compose network but not on the host, the same case as no ports at all.
#[test]
fn does_not_warn_on_expose_only() {
	let msgs =
		diagnostics_for("services:\n  db:\n    image: postgres\n    expose:\n      - \"5432\"\n");
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `host_ip: 127.0.0.1` is the long-form mirror of the
/// short-loopback form: restricted to the host, no warning.
#[test]
fn does_not_warn_on_long_port_with_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 5432\n        published: 5432\n        host_ip: 127.0.0.1\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `published` but no `host_ip` is the long-form mirror of
/// `5432:5432`: bind on every interface, warning fires.
#[test]
fn warns_on_long_port_without_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 80\n        published: 8080\n",
	);
	assert!(
		msgs.iter().any(|m| m.contains("service 'db'")
			&& m.contains("port 8080")
			&& m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `published: 8080` and `host_ip: 0.0.0.0` is the long-form
/// mirror of the explicit-all-interfaces short form: explicit bind, no warning.
#[test]
fn does_not_warn_on_long_port_with_explicit_all_interfaces_host_ip() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 80\n        published: 8080\n        host_ip: 0.0.0.0\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Long form with `target` but no `published` is the long-form mirror of
/// `expose:`: the port is exposed to peers but not published on the host,
/// so the warning has nothing to attach to.
#[test]
fn does_not_warn_on_long_port_without_published() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - target: 5432\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Short form with a `/tcp` protocol suffix still lacks a host IP, so the
/// warning fires just like the bare `8080:80`.
#[test]
fn warns_on_short_port_with_protocol_suffix() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"8080:80/tcp\"\n",
	);
	assert!(
		msgs.iter()
			.any(|m| m.contains("port 8080") && m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Short form with just a container port (`"80"`) is the short-form mirror
/// of `expose:`: not published on the host, so no warning.
#[test]
fn does_not_warn_on_short_container_only_port() {
	let msgs =
		diagnostics_for("services:\n  db:\n    image: postgres\n    ports:\n      - \"80\"\n");
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// IPv6 short form `[::1]:5432:5432` carries an explicit host IP, so the
/// bind is restricted and no warning is due.
#[test]
fn does_not_warn_on_ipv6_short_port() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"[::1]:5432:5432\"\n",
	);
	assert!(
		!msgs.iter().any(|m| m.contains("every interface")),
		"got: {msgs:?}"
	);
}

/// Two ports on the same service: one with IP, one without; only the
/// IP-less one warns.
#[test]
fn warns_only_for_ip_less_port_in_mixed_list() {
	let msgs = diagnostics_for(
		"services:\n  db:\n    image: postgres\n    ports:\n      - \"127.0.0.1:5432:5432\"\n      - \"8080:80\"\n",
	);
	let exposure_warnings: Vec<_> = msgs
		.iter()
		.filter(|m| m.contains("every interface"))
		.collect();
	assert_eq!(
		exposure_warnings.len(),
		1,
		"expected exactly one exposure warning; got: {exposure_warnings:?}"
	);
	assert!(
		exposure_warnings[0].contains("port 8080"),
		"warning should name the IP-less port; got: {:?}",
		exposure_warnings[0]
	);
}
