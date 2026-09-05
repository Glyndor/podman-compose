//! Network creation and per-network options for container specs.

use std::collections::HashMap;

use crate::compose::types::{ComposeFile, IpamConfig, Service, ServiceNetworkConfig};
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::{Namespace, PerNetworkOptions};
use crate::libpod::types::network::{LeaseRange, NetworkCreateRequest, Subnet};
use crate::libpod::API_PREFIX;

use super::Engine;

impl Engine {
	/// Pre-create every declared (non-external) network before containers start,
	/// stamping each with the `podup.project` label and applying driver/IPAM/label
	/// config. External networks are verified to already exist instead. An
	/// already-exists conflict on re-`up` is treated as success (idempotent).
	pub(super) async fn create_networks(&self, file: &ComposeFile) -> Result<()> {
		for (name, config) in &file.networks {
			let network_name = config
				.as_ref()
				.and_then(|c| c.name.as_deref())
				.map(|s| s.to_string())
				.unwrap_or_else(|| format!("{}_{}", self.project, name));

			let external = config.as_ref().and_then(|c| c.external).unwrap_or(false);
			if external {
				let external_name = config
					.as_ref()
					.and_then(|c| c.name.as_deref())
					.unwrap_or(name);
				self.ensure_external_exists("network", "networks", external_name)
					.await?;
				continue;
			}

			let driver = config
				.as_ref()
				.and_then(|c| c.driver.clone())
				.unwrap_or_else(|| "bridge".into());

			let mut labels: HashMap<String, String> = config
				.as_ref()
				.map(|c| c.labels.to_map())
				.unwrap_or_default();
			labels.insert("podup.project".to_string(), self.project.clone());

			let mut driver_opts: HashMap<String, String> = config
				.as_ref()
				.map(|c| c.driver_opts.clone())
				.unwrap_or_default();
			apply_default_isolation(&driver, &mut driver_opts);

			let ipam = config.as_ref().and_then(|c| c.ipam.as_ref());
			let subnets = ipam.map(build_subnets).unwrap_or_default();
			let ipam_options = ipam.map(build_ipam_options).unwrap_or_default();

			let request = NetworkCreateRequest {
				name: network_name.clone(),
				driver: Some(driver),
				internal: config.as_ref().and_then(|c| c.internal),
				attachable: config.as_ref().and_then(|c| c.attachable),
				ipv6_enabled: config.as_ref().and_then(|c| c.enable_ipv6),
				dns_enabled: Some(true),
				options: driver_opts,
				ipam_options,
				labels,
				subnets,
			};

			crate::ui::progress::start("Network", &network_name, "Creating");
			match self
				.client
				.post_json::<_, serde_json::Value>(
					&format!("{API_PREFIX}/networks/create"),
					&request,
				)
				.await
			{
				Ok(_) => crate::ui::progress_line("Network", &network_name, "Created"),
				// An existing network is not an error on re-`up`; accept any
				// already-exists conflict (network-create returns 409, but share
				// the same predicate as volume-create for consistency). The row
				// still needs to close — without an explicit closing verb the
				// live board leaves it spinning on `Creating` (#1347).
				Err(ref e) if e.is_already_exists() => {
					crate::ui::progress_line("Network", &network_name, "Exists");
				}
				Err(e) => return Err(ComposeError::Podman(e)),
			}
		}
		Ok(())
	}
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// Build the `PerNetworkOptions` for one service's attachment to a single
/// network: aliases, static IPv4/IPv6 and link-local IPs, MAC (per-network, else
/// `fallback_mac`), driver options (with `priority` folded in), and interface
/// name. The service name is always prepended as an alias unless already present,
/// so siblings resolve the service by name (compose DNS contract).
pub(super) fn build_per_network_options(
	service_name: &str,
	cfg: Option<&ServiceNetworkConfig>,
	fallback_mac: Option<&str>,
) -> PerNetworkOptions {
	let mut opts = PerNetworkOptions::default();

	if let Some(c) = cfg {
		opts.aliases = c.aliases.clone().unwrap_or_default();
		if let Some(ipv4) = &c.ipv4_address {
			opts.static_ips.push(ipv4.clone());
		}
		if let Some(ipv6) = &c.ipv6_address {
			opts.static_ips.push(ipv6.clone());
		}
		if !c.link_local_ips.is_empty() {
			opts.static_ips.extend(c.link_local_ips.clone());
		}
		let mac = c.mac_address.as_deref().or(fallback_mac);
		if let Some(m) = mac {
			opts.static_mac = Some(m.to_string());
		}
		// Forward per-attachment driver options. `priority` is surfaced by Compose
		// as a dedicated field but Podman consumes it as a driver option, so fold
		// it in alongside any explicit `driver_opts`.
		let mut driver_opts = c.driver_opts.clone();
		if let Some(prio) = c.priority {
			driver_opts.insert("priority".to_string(), prio.to_string());
		}
		if !driver_opts.is_empty() {
			opts.driver_opts = Some(driver_opts);
		}
		if let Some(iface) = &c.interface_name {
			opts.interface_name = Some(iface.clone());
		}
		// `gw_priority` has no Podman equivalent and is dropped. The user-facing
		// notice is emitted once, at parse time, by the compose diagnostics (see
		// internal/compose/diagnostics/ignored_fields.rs); re-emitting it here on
		// every engine build would double-warn, so no engine-time log is needed.
	} else if let Some(mac) = fallback_mac {
		opts.static_mac = Some(mac.to_string());
	}

	// A service is reachable by its service name on every network it joins
	// (compose-spec DNS contract). Register the service name as a network alias
	// unless the compose file already lists it, so siblings can resolve it by
	// name and not only by the container name or the auto-generated id alias.
	if !service_name.is_empty() && !opts.aliases.iter().any(|a| a == service_name) {
		opts.aliases.insert(0, service_name.to_string());
	}

	opts
}

/// The netavark option that stops a bridge network from reaching another one.
const ISOLATE_OPT: &str = "isolate";

/// Ask netavark to isolate a bridge network unless the compose file said
/// otherwise.
///
/// Docker isolates its bridge networks from each other and podman does not, so
/// without this a service reaches ports on a neighbouring network that were
/// never published. Measured on 2026-08-20 with the same experiment on both
/// engines — a listener on an unpublished port in network B, and a container in
/// network A dialling its address:
///
/// | engine | result |
/// |---|---|
/// | docker 29.6.2 | refused |
/// | podman 5.7.0 | connected |
///
/// The control matters: inside the *same* network docker connects, so the
/// refusal is isolation rather than a broken dial.
///
/// Also measured on podman 5.7.0 with the option set: traffic within one
/// network still flows (compose keeps working) and outbound internet still
/// works.
///
/// **The option only bites when both networks carry it.** Measured three ways:
/// isolated → isolated is refused, isolated → plain connects, and plain →
/// isolated connects. So this does not harden a network against the world; it
/// makes networks that share the default stop seeing each other. Since podup
/// now sets it on every bridge network it creates, the effect is that two
/// podup projects no longer reach each other's unpublished ports — verified
/// end to end with two projects — while a network created outside podup
/// without the option still can. Being a default rather than opt-in is the
/// whole point: one project opting in would protect nobody.
///
/// A compose file that sets `isolate` in `driver_opts` wins, including setting
/// it to `false`, which is the escape hatch for a project that deliberately
/// spans networks.
fn apply_default_isolation(driver: &str, opts: &mut HashMap<String, String>) {
	if driver != "bridge" {
		return;
	}
	opts.entry(ISOLATE_OPT.to_string())
		.or_insert_with(|| "true".to_string());
}

/// Resolve a service's networking into a netns `Namespace` and per-network
/// options. An explicit `network_mode` wins and yields a namespace with no
/// per-network options (`container:`/`service:` reuse another container's netns,
/// the service form resolved to its target container name). Otherwise each
/// declared network is mapped to its options and `bridge` netns is used (libpod
/// requires `netns=bridge` when explicit networks are attached).
pub(super) fn resolve_network_mode(
	service_name: &str,
	service: &Service,
	file: &ComposeFile,
	project: &str,
) -> (Option<Namespace>, HashMap<String, PerNetworkOptions>) {
	if let Some(mode) = &service.network_mode {
		let ns = if let Some(id) = mode.strip_prefix("container:") {
			Namespace::container(id)
		} else if let Some(svc_name) = mode.strip_prefix("service:") {
			let cname = file
				.services
				.get(svc_name)
				.map(|s| resolve_target_container_name(svc_name, s, project))
				.unwrap_or_else(|| svc_name.to_string());
			Namespace::container(cname)
		} else {
			if mode == "bridge" {
				// docker-compose attaches to Docker's shared default `bridge`;
				// Podman reads `--network bridge` as a fresh isolated bridge netns,
				// so project siblings are unreachable. Warn here (at container
				// create) so every path — CLI, library, embedded — surfaces it, not
				// only the parse-time diagnostics.
				tracing::warn!(
					"service '{service_name}': network_mode 'bridge' attaches to a fresh \
					 isolated bridge under Podman, not Docker's shared default bridge, so \
					 project siblings are unreachable; declare a shared `networks:` entry instead"
				);
			}
			Namespace::new(mode)
		};
		return (Some(ns), HashMap::new());
	}

	let network_names = service.networks.names();
	let networks: HashMap<String, PerNetworkOptions> = network_names
		.iter()
		.map(|net_name| {
			let full = resolve_network_name(net_name, file, project);
			let opts = build_per_network_options(
				service_name,
				service.networks.config_for(net_name),
				service.mac_address.as_deref(),
			);
			(full, opts)
		})
		.collect();

	// Podman libpod requires netns=bridge when explicit networks are used.
	let netns = (!networks.is_empty()).then(|| Namespace::new("bridge"));

	(netns, networks)
}

/// Resolve the container name that a `network_mode: service:<name>` reference
/// should attach to.
///
/// An explicit `container_name` is honoured verbatim. Otherwise the auto-generated
/// replicas are always index-suffixed `{project}-{svc}-1..N` (there is no
/// base-named container at any replica count), so we point at replica `-1`,
/// matching docker-compose, which attaches `network_mode: service:` references to
/// the first replica.
fn resolve_target_container_name(svc_name: &str, service: &Service, project: &str) -> String {
	if let Some(name) = &service.container_name {
		return name.clone();
	}
	format!("{project}-{svc_name}-1")
}

/// Resolve the actual network name on the host for a compose network key.
///
/// `pub` so `startup::config_render` (in the binary crate) can call the same
/// function `up` uses, rather than duplicating the resolution rule. The rule
/// is small but every branch (explicit `name:`, `external: true`, the
/// `<project>_<key>` default) has a footgun if a second copy drifts. The
/// crate-level docs (`podup::resolve_network_name`) call out the reuse intent.
pub fn resolve_network_name(network: &str, file: &ComposeFile, project: &str) -> String {
	match file.networks.get(network).and_then(|c| c.as_ref()) {
		Some(cfg) => {
			if let Some(name) = cfg.name.as_deref() {
				name.to_string()
			} else if cfg.external.unwrap_or(false) {
				network.to_string()
			} else {
				format!("{project}_{network}")
			}
		}
		None => format!("{project}_{network}"),
	}
}

fn build_subnets(ipam: &IpamConfig) -> Vec<Subnet> {
	// aux_addresses are reported by the parse-time diagnostics pass (they are not
	// supported by Podman), so the drop is surfaced there rather than logged here.
	ipam.config
		.iter()
		.map(|pool| Subnet {
			subnet: pool.subnet.clone(),
			gateway: pool.gateway.clone(),
			lease_range: pool.ip_range.as_deref().and_then(lease_range_from_cidr),
		})
		.collect()
}

/// Translate `ipam.driver` and `ipam.options` into Podman's `ipam_options` map.
fn build_ipam_options(ipam: &IpamConfig) -> HashMap<String, String> {
	let mut opts = ipam.options.clone();
	if let Some(driver) = &ipam.driver {
		opts.insert("driver".to_string(), driver.clone());
	}
	opts
}

/// Convert a compose `ip_range` CIDR into a Podman lease range (the usable
/// host range of the CIDR). Returns `None` for an unparseable CIDR.
fn lease_range_from_cidr(cidr: &str) -> Option<LeaseRange> {
	use std::net::{Ipv4Addr, Ipv6Addr};

	let (addr, prefix) = cidr.split_once('/')?;
	let prefix: u8 = prefix.parse().ok()?;

	if let Ok(v4) = addr.parse::<Ipv4Addr>() {
		if prefix > 32 {
			return None;
		}
		let mask = if prefix == 0 {
			0
		} else {
			u32::MAX << (32 - prefix)
		};
		let base = u32::from(v4) & mask;
		let last = base | !mask;
		// Reserve network and broadcast addresses for non-point-to-point ranges.
		let (start, end) = if prefix >= 31 {
			(base, last)
		} else {
			(base + 1, last - 1)
		};
		return Some(LeaseRange {
			start_ip: Some(Ipv4Addr::from(start).to_string()),
			end_ip: Some(Ipv4Addr::from(end).to_string()),
		});
	}

	if let Ok(v6) = addr.parse::<Ipv6Addr>() {
		if prefix > 128 {
			return None;
		}
		let mask = if prefix == 0 {
			0
		} else {
			u128::MAX << (128 - prefix)
		};
		let base = u128::from(v6) & mask;
		let last = base | !mask;
		return Some(LeaseRange {
			start_ip: Some(Ipv6Addr::from(base).to_string()),
			end_ip: Some(Ipv6Addr::from(last).to_string()),
		});
	}

	None
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
