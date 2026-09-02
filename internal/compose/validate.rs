//! Validation of a fully parsed and merged compose file.
//!
//! Two entry points live here. [`validate_config`] backs the `config` subcommand,
//! applying the same cross-reference and well-formedness checks
//! `docker compose config` performs and that the mutating commands would
//! otherwise only surface later (at `resolve_order` time, when Podman rejects a
//! bad port, or when an undeclared volume/network reaches the runtime). Running
//! them up front means `config` reports the divergence at exit non-zero instead
//! of echoing the file verbatim.
//!
//! [`validate`] is the semantic consistency pass run automatically after parsing
//! and merging: it rejects files that deserialize cleanly but are semantically
//! contradictory (e.g. a service that declares both `network_mode` and
//! `networks`, or an `external: true` network that also sets creation-time
//! attributes). docker-compose errors on these at config time; podup used to
//! accept them and then silently pick one interpretation, with the live engine
//! and the Quadlet exporter diverging on which one. Failing fast here keeps
//! `config`, `up`, and `generate` in agreement.

use crate::compose::order::resolve_order;
use crate::compose::types::{ComposeFile, PortMapping, VolumeMount, VolumeType};
use crate::error::{ComposeError, Result};
use crate::ports::parse_ports;

/// Validate a parsed compose file the way `docker compose config` does.
///
/// Checks, in order: at least one service is defined; every service declares an
/// `image:` or `build:`; service names use the compose charset; published/target
/// ports are in range; every referenced named volume and network is declared at
/// the top level; and the `depends_on` graph is acyclic with no dangling
/// required dependency. Returns the first violation found.
pub fn validate_config(file: &ComposeFile) -> Result<()> {
	// An empty file, a missing `services:` key, or `services: {}` is not a valid
	// project — `docker compose config` errors with "no service selected".
	if file.services.is_empty() {
		return Err(ComposeError::Unsupported(
			"no services defined in compose file".to_string(),
		));
	}

	for (name, svc) in &file.services {
		validate_service_name(name)?;
		if svc.image.is_none() && svc.build.is_none() {
			return Err(ComposeError::NoImageOrBuild(name.clone()));
		}
		validate_ports(name, &svc.ports)?;
		validate_network_refs(name, file, svc)?;
		validate_volume_refs(name, file, svc)?;
	}

	// Reject `depends_on` cycles and dangling required dependencies, matching the
	// mutating commands (which run `resolve_order` before they start anything).
	resolve_order(file)?;
	Ok(())
}

/// Reject a service name that is empty or uses characters outside the compose
/// charset (`[a-zA-Z0-9._-]`). Spaces and punctuation like `!` are rejected.
fn validate_service_name(name: &str) -> Result<()> {
	let ok = !name.is_empty()
		&& name
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
	if ok {
		Ok(())
	} else {
		Err(ComposeError::Unsupported(format!(
			"service name {name:?} is invalid: use only ASCII letters, digits, '.', '_', '-'"
		)))
	}
}

/// Range-check every port a service publishes. `parse_ports` rejects values that
/// do not fit a `u16` (e.g. `99999`); on top of that a container or host port of
/// `0` is rejected here, since a valid published/target port is `1`–`65535`.
fn validate_ports(service: &str, ports: &[PortMapping]) -> Result<()> {
	for parsed in parse_ports(ports)? {
		if parsed.container_port == 0 || parsed.host_port == Some(0) {
			return Err(ComposeError::InvalidPort(format!(
				"service '{service}' has a port of 0; ports must be in 1-65535"
			)));
		}
	}
	Ok(())
}

/// Every network a service joins must be declared in the top-level `networks:`
/// map (the implicit `default` network is synthesized before this runs, so a
/// bare service still validates). `network_mode:` services declare no networks.
fn validate_network_refs(
	service: &str,
	file: &ComposeFile,
	svc: &crate::compose::types::Service,
) -> Result<()> {
	for net in svc.networks.names() {
		if !file.networks.contains_key(&net) {
			return Err(ComposeError::Unsupported(format!(
				"service '{service}' refers to undefined network '{net}'; declare it under the \
				 top-level 'networks:' key"
			)));
		}
	}
	Ok(())
}

/// Every *named* volume a service mounts must be declared in the top-level
/// `volumes:` map. Bind mounts (host paths) and anonymous volumes carry no
/// top-level declaration and are skipped.
fn validate_volume_refs(
	service: &str,
	file: &ComposeFile,
	svc: &crate::compose::types::Service,
) -> Result<()> {
	for mount in &svc.volumes {
		let named = match mount {
			VolumeMount::Short(s) => short_named_volume(s),
			VolumeMount::Long {
				volume_type: VolumeType::Volume,
				source: Some(src),
				..
			} => Some(src.as_str()),
			VolumeMount::Long { .. } => None,
		};
		if let Some(name) = named {
			if !file.volumes.contains_key(name) {
				return Err(ComposeError::Unsupported(format!(
					"service '{service}' refers to undefined volume '{name}'; declare it under the \
					 top-level 'volumes:' key"
				)));
			}
		}
	}
	Ok(())
}

/// Extract the named-volume reference from a short-form `source:target[:opts]`
/// mount, or `None` when it is a host-path bind or an anonymous volume.
///
/// Mirrors the engine's own classification: a source starting with `/`, `.` or
/// `~`, or a Windows drive prefix (`C:`), is a bind, not a named volume; a
/// single token with no target is an anonymous volume.
fn short_named_volume(spec: &str) -> Option<&str> {
	let (src, _rest) = spec.split_once(':')?;
	if src.is_empty()
		|| src.starts_with('/')
		|| src.starts_with('.')
		|| src.starts_with('~')
		|| is_windows_drive(src)
	{
		return None;
	}
	Some(src)
}

/// Whether `src` is exactly a Windows drive letter (e.g. `C`), meaning the colon
/// after it is part of a host path rather than the `source:target` separator.
fn is_windows_drive(src: &str) -> bool {
	let b = src.as_bytes();
	b.len() == 1 && b[0].is_ascii_alphabetic()
}

/// Validate the semantic consistency of a resolved compose file. Returns the
/// first error found, matching docker-compose's fail-at-config-time behaviour.
///
/// Run only on the interpolated file: with `--no-interpolate` the values may
/// still contain literal `${VAR}` placeholders, which cannot be meaningfully
/// range- or reference-checked.
pub(super) fn validate(file: &ComposeFile) -> Result<()> {
	validate_services(file)?;
	validate_networks(file)?;
	Ok(())
}

/// Per-service checks: the `network_mode`/`networks` mutual exclusion, every
/// network reference resolving to a declared (or external) network, and that the
/// `ports:` entries parse and are in range.
fn validate_services(file: &ComposeFile) -> Result<()> {
	for (name, service) in &file.services {
		let attached = service.networks.names();
		if service.network_mode.is_some() {
			// docker-compose: "network_mode" and "networks" cannot be combined.
			// The live engine silently honours network_mode and drops the declared
			// networks; Quadlet emits both, producing a contradictory unit.
			if !attached.is_empty() {
				return Err(ComposeError::Unsupported(format!(
					"service '{name}' sets both 'network_mode' and 'networks', which are \
					 mutually exclusive; keep one"
				)));
			}
		} else {
			// Every referenced network must be declared at the top level (or be the
			// synthesized `default`). An undefined reference is a config error in
			// docker-compose; podup otherwise prefixes it on the engine path while
			// the Quadlet exporter emits the raw name, a cross-project attach risk.
			for net in &attached {
				// `default` is the implicit project network docker-compose always
				// provides, so an explicit reference to it is valid even without a
				// top-level entry; everything else must be declared.
				if net != "default" && !file.networks.contains_key(net) {
					return Err(ComposeError::Unsupported(format!(
						"service '{name}' refers to undefined network '{net}'; declare it \
						 under the top-level 'networks:' or mark it external"
					)));
				}
			}
		}

		// Surface a malformed/out-of-range port at config time rather than letting
		// it slip through to a podman create error at run time.
		parse_ports(&service.ports)?;
	}
	Ok(())
}

/// Top-level network checks: an `external: true` network must not also carry
/// creation-time attributes (driver, IPAM, internal, …), which podman cannot
/// apply to a pre-existing network and docker-compose rejects.
fn validate_networks(file: &ComposeFile) -> Result<()> {
	for (name, cfg) in &file.networks {
		let Some(cfg) = cfg else { continue };
		if cfg.external != Some(true) {
			continue;
		}
		let mut conflicts = Vec::new();
		if cfg.driver.is_some() {
			conflicts.push("driver");
		}
		if !cfg.driver_opts.is_empty() {
			conflicts.push("driver_opts");
		}
		if cfg.internal.is_some() {
			conflicts.push("internal");
		}
		if cfg.attachable.is_some() {
			conflicts.push("attachable");
		}
		if cfg.enable_ipv6.is_some() {
			conflicts.push("enable_ipv6");
		}
		if cfg.enable_ipv4.is_some() {
			conflicts.push("enable_ipv4");
		}
		if cfg.ipam.is_some() {
			conflicts.push("ipam");
		}
		if !conflicts.is_empty() {
			return Err(ComposeError::Unsupported(format!(
				"network '{name}' is external but also sets {}; an external network is \
				 used as-is and these attributes cannot be applied to it",
				conflicts.join(", ")
			)));
		}
	}
	Ok(())
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
