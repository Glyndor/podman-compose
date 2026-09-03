//! Port mapping parser.
//!
//! Handles all docker-compose port format variants and converts them to
//! libpod `PortMapping` structures.

use serde::Serialize;

use crate::compose::types::{PortMapping, StringOrU16};
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::PortMapping as LibpodPortMapping;

/// A parsed, normalized port binding.
#[derive(Debug, Clone, Serialize)]
pub struct ParsedPort {
	/// Container port number.
	pub container_port: u16,
	/// Protocol (`tcp`, `udp`, `sctp`).
	pub protocol: String,
	/// Host IP (may be empty to mean all interfaces).
	pub host_ip: String,
	/// Host port (`None` means random / ephemeral; `Some(0)` means runtime-assigned).
	pub host_port: Option<u16>,
}

/// Parse all port mappings in a service, expanding ranges.
pub fn parse_ports(ports: &[PortMapping]) -> Result<Vec<ParsedPort>> {
	let mut result = Vec::new();
	for mapping in ports {
		result.extend(parse_one(mapping)?);
	}
	Ok(result)
}

/// Convert parsed ports into libpod `PortMapping` entries for `SpecGenerator`.
pub fn to_libpod(ports: &[ParsedPort]) -> Vec<LibpodPortMapping> {
	ports
		.iter()
		.map(|p| LibpodPortMapping {
			container_port: p.container_port,
			host_port: p.host_port,
			host_ip: if p.host_ip.is_empty() {
				String::new()
			} else {
				p.host_ip.clone()
			},
			protocol: p.protocol.clone(),
			range: None,
		})
		.collect()
}

// ---------------------------------------------------------------------------
// Internal
// ---------------------------------------------------------------------------

fn parse_one(mapping: &PortMapping) -> Result<Vec<ParsedPort>> {
	match mapping {
		PortMapping::Short(s) => parse_short(s),
		PortMapping::Long {
			target,
			published,
			protocol,
			host_ip,
			..
		} => {
			let proto = protocol.clone().unwrap_or_else(|| "tcp".into());
			validate_protocol(&proto, &format!("{target}/{proto}"))?;
			let hip = host_ip.clone().unwrap_or_default();
			let host_port = published
				.as_ref()
				.map(|p| match p {
					StringOrU16::Number(n) => port_in_range(*n, &n.to_string()),
					StringOrU16::String(s) => {
						let n: u32 = s.parse().map_err(|_| {
							ComposeError::InvalidPort(format!("invalid published port: {s}"))
						})?;
						port_in_range(n, s)
					}
				})
				.transpose()?;
			Ok(vec![ParsedPort {
				container_port: *target,
				protocol: proto,
				host_ip: hip,
				host_port,
			}])
		}
	}
}

/// Parse a short-form port string.
///
/// Formats:
/// - `container`
/// - `container/proto`
/// - `host:container`
/// - `host:container/proto`
/// - `ip:host:container` (ip may be IPv4 or `[ipv6]`)
/// - `ip:host:container/proto`
/// - `host_start-host_end:container_start-container_end`
fn parse_short(s: &str) -> Result<Vec<ParsedPort>> {
	// Split off protocol suffix.
	let (rest, proto) = if let Some(idx) = s.rfind('/') {
		(&s[..idx], s[idx + 1..].to_string())
	} else {
		(s, "tcp".to_string())
	};
	validate_protocol(&proto, s)?;

	// IPv6 form: `[::1]:host:container` or `[::1]:container`.
	if let Some(rest) = rest.strip_prefix('[') {
		let close = rest
			.find(']')
			.ok_or_else(|| ComposeError::InvalidPort(format!("unclosed `[` in {s}")))?;
		let ip = &rest[..close];
		let after = &rest[close + 1..];
		let after = after.strip_prefix(':').unwrap_or(after);
		return parse_with_ip(ip, after, &proto, s);
	}

	// Count colons to determine format.
	let colon_count = rest.chars().filter(|&c| c == ':').count();

	match colon_count {
		0 => {
			// Just container port (possibly a range).
			let ports = expand_port_range(rest)?;
			Ok(ports
				.into_iter()
				.map(|cp| ParsedPort {
					container_port: cp,
					protocol: proto.clone(),
					host_ip: String::new(),
					host_port: None,
				})
				.collect())
		}
		1 => {
			let (left, right) = split_last_colon(rest);
			let host_ports = expand_port_range(left)?;
			let container_ports = expand_port_range(right)?;
			let host_ports = expand_single_host_port(host_ports, container_ports.len(), s)?;
			if host_ports.len() != container_ports.len() {
				return Err(ComposeError::InvalidPort(format!(
					"port range mismatch: {s}"
				)));
			}
			Ok(host_ports
				.into_iter()
				.zip(container_ports)
				.map(|(hp, cp)| ParsedPort {
					container_port: cp,
					protocol: proto.clone(),
					host_ip: String::new(),
					host_port: Some(hp),
				})
				.collect())
		}
		_ => {
			let parts: Vec<&str> = rest.splitn(3, ':').collect();
			if parts.len() < 3 {
				return Err(ComposeError::InvalidPort(format!("invalid port spec: {s}")));
			}
			parse_with_ip(parts[0], &format!("{}:{}", parts[1], parts[2]), &proto, s)
		}
	}
}

/// Parse the `host[:container]` portion when an explicit IP prefix is present.
fn parse_with_ip(ip: &str, after: &str, proto: &str, full: &str) -> Result<Vec<ParsedPort>> {
	if let Some((left, right)) = after.split_once(':') {
		let host_ports = expand_port_range(left)?;
		let container_ports = expand_port_range(right)?;
		let host_ports = expand_single_host_port(host_ports, container_ports.len(), full)?;
		if host_ports.len() != container_ports.len() {
			return Err(ComposeError::InvalidPort(format!(
				"port range mismatch: {full}"
			)));
		}
		Ok(host_ports
			.into_iter()
			.zip(container_ports)
			.map(|(hp, cp)| ParsedPort {
				container_port: cp,
				protocol: proto.to_string(),
				host_ip: ip.to_string(),
				host_port: Some(hp),
			})
			.collect())
	} else {
		let cp = parse_port_num(after)
			.map_err(|_| ComposeError::InvalidPort(format!("bad port: {full}")))?;
		Ok(vec![ParsedPort {
			container_port: cp,
			protocol: proto.to_string(),
			host_ip: ip.to_string(),
			host_port: None,
		}])
	}
}

/// Split at the LAST colon (to avoid splitting IPv6 addresses incorrectly).
fn split_last_colon(s: &str) -> (&str, &str) {
	if let Some(idx) = s.rfind(':') {
		(&s[..idx], &s[idx + 1..])
	} else {
		("", s)
	}
}

/// When `host_ports` contains exactly one port and `container_count > 1`, expand
/// the host side to a range starting at `host_ports[0]` (docker-compose semantics
/// for `8080:80-82` → 8080→80, 8081→81, 8082→82).
fn expand_single_host_port(
	host_ports: Vec<u16>,
	container_count: usize,
	spec: &str,
) -> Result<Vec<u16>> {
	if host_ports.len() == 1 && container_count > 1 {
		let start = host_ports[0];
		let end = start
			.checked_add((container_count - 1) as u16)
			.ok_or_else(|| {
				ComposeError::InvalidPort(format!("host port range overflow: {spec}"))
			})?;
		Ok((start..=end).collect())
	} else {
		Ok(host_ports)
	}
}

pub(crate) const MAX_PORT_RANGE: usize = 1024;

/// The set of transport protocols podman accepts for a published port. A value
/// outside this set is rejected at config time rather than passed verbatim to
/// podman, which would only surface as an opaque create-time error.
const VALID_PROTOCOLS: [&str; 3] = ["tcp", "udp", "sctp"];

/// Validate a port's protocol suffix against the `tcp`/`udp`/`sctp` allow-list.
fn validate_protocol(proto: &str, full: &str) -> Result<()> {
	if VALID_PROTOCOLS.contains(&proto) {
		Ok(())
	} else {
		Err(ComposeError::InvalidPort(format!(
			"unsupported protocol '{proto}' in '{full}'; use tcp, udp, or sctp"
		)))
	}
}

/// Range-check a numeric port and narrow it to `u16`. Surfaces an out-of-range
/// value (e.g. `99999`) as a clear config error so the short and long port forms
/// fail the same way instead of overflowing or leaking a serde enum message.
fn port_in_range(n: u32, label: &str) -> Result<u16> {
	u16::try_from(n)
		.map_err(|_| ComposeError::InvalidPort(format!("port out of range (1-65535): {label}")))
}

/// Parse a single port number, range-checked against 1-65535.
fn parse_port_num(s: &str) -> Result<u16> {
	let n: u32 = s
		.parse()
		.map_err(|_| ComposeError::InvalidPort(format!("bad port: {s}")))?;
	port_in_range(n, s)
}

/// Expand `start-end` or a single port string.
fn expand_port_range(s: &str) -> Result<Vec<u16>> {
	let s = s.trim();
	if let Some(idx) = s.find('-') {
		let start = parse_port_num(&s[..idx])?;
		let end = parse_port_num(&s[idx + 1..])?;
		if start > end {
			return Err(ComposeError::InvalidPort(format!(
				"start > end in range: {s}"
			)));
		}
		let count = (end as usize) - (start as usize) + 1;
		if count > MAX_PORT_RANGE {
			return Err(ComposeError::InvalidPort(format!(
				"port range too large ({count} ports, max {MAX_PORT_RANGE}): {s}"
			)));
		}
		Ok((start..=end).collect())
	} else {
		Ok(vec![parse_port_num(s)?])
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "ports_tests.rs"]
mod tests;
