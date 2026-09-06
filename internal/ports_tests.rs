use super::*;
use crate::compose::types::{PortMapping, StringOrU16};

fn short(s: &str) -> PortMapping {
	PortMapping::Short(s.to_string())
}

fn parse_one_short(s: &str) -> Vec<ParsedPort> {
	parse_ports(&[short(s)]).unwrap()
}

// Container port only

#[test]
fn container_port_only() {
	let ports = parse_one_short("80");
	assert_eq!(ports.len(), 1);
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[0].protocol, "tcp");
	assert_eq!(ports[0].host_ip, "");
	assert!(ports[0].host_port.is_none());
}

#[test]
fn container_port_with_explicit_protocol() {
	let ports = parse_one_short("53/udp");
	assert_eq!(ports[0].container_port, 53);
	assert_eq!(ports[0].protocol, "udp");
}

// host:container

#[test]
fn host_colon_container() {
	let ports = parse_one_short("8080:80");
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[0].host_port, Some(8080));
	assert_eq!(ports[0].host_ip, "");
}

// ip:host:container

#[test]
fn ip_host_container() {
	let ports = parse_one_short("127.0.0.1:8080:80");
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[0].host_port, Some(8080));
	assert_eq!(ports[0].host_ip, "127.0.0.1");
}

#[test]
fn ipv6_bracketed() {
	let ports = parse_one_short("[::1]:8080:80");
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[0].host_port, Some(8080));
	assert_eq!(ports[0].host_ip, "::1");
}

#[test]
fn ipv6_bracketed_container_only_has_no_host_port() {
	// `[ip]:container` (no published host port) binds the IPv6 address and
	// lets Podman assign the host port, the no-host-port arm of parse_with_ip.
	let ports = parse_one_short("[::1]:80");
	assert_eq!(ports.len(), 1);
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[0].host_ip, "::1");
	assert!(ports[0].host_port.is_none());
}

#[test]
fn malformed_three_part_short_is_error() {
	// More than one colon but a missing third segment is rejected rather than
	// silently mis-parsed.
	assert!(parse_ports(&[short("1.2.3.4:8080:")]).is_err());
}

// Range expansion

#[test]
fn container_port_range() {
	let ports = parse_one_short("80-82");
	assert_eq!(ports.len(), 3);
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[2].container_port, 82);
}

#[test]
fn host_range_to_container_range() {
	let ports = parse_one_short("8080-8082:80-82");
	assert_eq!(ports.len(), 3);
	assert_eq!(ports[0].host_port, Some(8080));
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[2].host_port, Some(8082));
	assert_eq!(ports[2].container_port, 82);
}

#[test]
fn single_host_expanded_for_container_range() {
	let ports = parse_one_short("8080:80-82");
	assert_eq!(ports.len(), 3);
	assert_eq!(ports[0].host_port, Some(8080));
	assert_eq!(ports[1].host_port, Some(8081));
	assert_eq!(ports[2].host_port, Some(8082));
}

// Error cases

#[test]
fn range_start_greater_than_end_is_error() {
	assert!(parse_ports(&[short("85-80")]).is_err());
}

#[test]
fn range_too_large_is_error() {
	let big = format!("1-{}", MAX_PORT_RANGE + 1);
	assert!(parse_ports(&[short(&big)]).is_err());
}

#[test]
fn invalid_port_string_is_error() {
	assert!(parse_ports(&[short("abc")]).is_err());
}

#[test]
fn invalid_protocol_suffix_is_rejected() {
	// A protocol outside tcp/udp/sctp is a config error, not something to pass
	// verbatim to podman.
	let err = parse_ports(&[short("80/banana")]).unwrap_err();
	assert!(err.to_string().contains("banana"), "got: {err}");
	assert!(parse_ports(&[short("53/udp")]).is_ok());
	assert!(parse_ports(&[short("9/sctp")]).is_ok());
}

#[test]
fn out_of_range_short_port_reports_range() {
	// 99999 overflows the 1-65535 port space; surface a clear range error
	// rather than a generic parse failure.
	let err = parse_ports(&[short("99999:80")]).unwrap_err();
	assert!(err.to_string().contains("out of range"), "got: {err}");
}

#[test]
fn out_of_range_long_published_reports_range() {
	// The numeric long form deserializes (u32) and is range-checked here, so the
	// user sees the same clear message as the short form instead of a serde
	// untagged-enum error.
	let mapping = PortMapping::Long {
		target: 80,
		published: Some(StringOrU16::Number(99999)),
		protocol: None,
		host_ip: None,
		mode: None,
		app_protocol: None,
		name: None,
	};
	let err = parse_ports(&[mapping]).unwrap_err();
	assert!(err.to_string().contains("out of range"), "got: {err}");
}

#[test]
fn long_form_invalid_protocol_is_rejected() {
	let mapping = PortMapping::Long {
		target: 80,
		published: Some(StringOrU16::Number(8080)),
		protocol: Some("banana".to_string()),
		host_ip: None,
		mode: None,
		app_protocol: None,
		name: None,
	};
	assert!(parse_ports(&[mapping]).is_err());
}

#[test]
fn unclosed_ipv6_bracket_is_error() {
	assert!(parse_ports(&[short("[::1:80")]).is_err());
}

#[test]
fn mismatched_host_and_container_ranges_is_error() {
	// A two-port host range cannot map onto a three-port container range.
	let err = parse_ports(&[short("8080-8081:80-82")]).unwrap_err();
	assert!(err.to_string().contains("mismatch"));
}

#[test]
fn ip_with_mismatched_ranges_is_error() {
	let err = parse_ports(&[short("127.0.0.1:8080-8081:80-82")]).unwrap_err();
	assert!(err.to_string().contains("mismatch"));
}

#[test]
fn single_host_port_range_overflow_is_error() {
	// Expanding a single host port across a container range that would carry
	// it past u16::MAX is rejected rather than wrapping.
	let err = parse_ports(&[short("65535:80-81")]).unwrap_err();
	assert!(err.to_string().contains("overflow"));
}

// Long form

#[test]
fn long_form_with_published() {
	let mapping = PortMapping::Long {
		target: 80,
		published: Some(StringOrU16::Number(8080)),
		protocol: Some("tcp".to_string()),
		host_ip: Some("0.0.0.0".to_string()),
		mode: None,
		app_protocol: None,
		name: None,
	};
	let ports = parse_ports(&[mapping]).unwrap();
	assert_eq!(ports[0].container_port, 80);
	assert_eq!(ports[0].host_port, Some(8080));
	assert_eq!(ports[0].host_ip, "0.0.0.0");
}

#[test]
fn long_form_no_published_defaults_to_none() {
	let mapping = PortMapping::Long {
		target: 80,
		published: None,
		protocol: None,
		host_ip: None,
		mode: None,
		app_protocol: None,
		name: None,
	};
	let ports = parse_ports(&[mapping]).unwrap();
	assert!(ports[0].host_port.is_none());
	assert_eq!(ports[0].protocol, "tcp");
}

// to_libpod

#[test]
fn to_libpod_produces_port_mapping() {
	let ports = parse_one_short("8080:80");
	let mappings = to_libpod(&ports);
	assert_eq!(mappings.len(), 1);
	assert_eq!(mappings[0].container_port, 80);
	assert_eq!(mappings[0].host_port, Some(8080));
	assert_eq!(mappings[0].protocol, "tcp");
}

#[test]
fn to_libpod_port_zero_passes_through() {
	let ports = vec![ParsedPort {
		container_port: 80,
		protocol: "tcp".to_string(),
		host_ip: String::new(),
		host_port: Some(0),
	}];
	let mappings = to_libpod(&ports);
	assert_eq!(mappings[0].host_port, Some(0));
}

#[test]
fn to_libpod_no_host_port_is_none() {
	let ports = parse_one_short("80");
	let mappings = to_libpod(&ports);
	assert_eq!(mappings[0].host_port, None);
}
