use super::*;
use crate::compose::types::{ComposeFile, NetworkConfig, Service};

fn empty_file() -> ComposeFile {
	ComposeFile::default()
}

fn file_with_named_network(key: &str, name: &str) -> ComposeFile {
	let cfg = NetworkConfig {
		name: Some(name.to_string()),
		..Default::default()
	};
	let mut file = empty_file();
	file.networks.insert(key.to_string(), Some(cfg));
	file
}

#[test]
fn resolve_network_name_key_not_found_prefixes_project() {
	let file = empty_file();
	assert_eq!(resolve_network_name("mynet", &file, "proj"), "proj_mynet");
}

#[test]
fn resolve_network_name_uses_config_name_over_prefix() {
	let file = file_with_named_network("mynet", "custom-net-name");
	assert_eq!(
		resolve_network_name("mynet", &file, "proj"),
		"custom-net-name"
	);
}

#[test]
fn resolve_network_name_external_uses_key_not_prefix() {
	let cfg = NetworkConfig {
		external: Some(true),
		..Default::default()
	};
	let mut file = empty_file();
	file.networks.insert("shared".to_string(), Some(cfg));
	assert_eq!(resolve_network_name("shared", &file, "proj"), "shared");
}

#[test]
fn resolve_network_mode_explicit_mode() {
	let svc = Service {
		network_mode: Some("host".to_string()),
		..Default::default()
	};
	let file = empty_file();
	let (ns, nets) = resolve_network_mode("web", &svc, &file, "proj");
	assert!(ns.is_some());
	assert_eq!(ns.unwrap().nsmode, "host");
	assert!(nets.is_empty());
}

fn file_with_service(svc_name: &str, svc: Service) -> ComposeFile {
	let mut file = empty_file();
	file.services.insert(svc_name.to_string(), svc);
	file
}

#[test]
fn network_mode_service_single_replica_uses_first_replica_name() {
	// Auto-generated container names are always index-suffixed, so even a
	// single-replica `network_mode: service:` target attaches to `-1`.
	let target = Service::default();
	let file = file_with_service("db", target);
	let svc = Service {
		network_mode: Some("service:db".to_string()),
		..Default::default()
	};
	let (ns, _) = resolve_network_mode("web", &svc, &file, "proj");
	let ns = ns.unwrap();
	assert_eq!(ns.nsmode, "container");
	assert_eq!(ns.value.as_deref(), Some("proj-db-1"));
}

#[test]
fn network_mode_service_scaled_replicas_resolves_replica_one() {
	// `scale:`/`deploy.replicas` > 1 means the base name does not exist —
	// docker-compose attaches to replica `-1`.
	let target = Service {
		scale: Some(3),
		..Default::default()
	};
	let file = file_with_service("db", target);
	let svc = Service {
		network_mode: Some("service:db".to_string()),
		..Default::default()
	};
	let (ns, _) = resolve_network_mode("web", &svc, &file, "proj");
	assert_eq!(ns.unwrap().value.as_deref(), Some("proj-db-1"));
}

#[test]
fn network_mode_service_deploy_replicas_resolves_replica_one() {
	use crate::compose::types::DeployConfig;
	let target = Service {
		deploy: Some(DeployConfig {
			replicas: Some(2),
			..Default::default()
		}),
		..Default::default()
	};
	let file = file_with_service("db", target);
	let svc = Service {
		network_mode: Some("service:db".to_string()),
		..Default::default()
	};
	let (ns, _) = resolve_network_mode("web", &svc, &file, "proj");
	assert_eq!(ns.unwrap().value.as_deref(), Some("proj-db-1"));
}

#[test]
fn network_mode_service_container_name_wins_over_replica() {
	// An explicit container_name is honoured verbatim even when scaled.
	let target = Service {
		scale: Some(4),
		container_name: Some("custom-db".to_string()),
		..Default::default()
	};
	let file = file_with_service("db", target);
	let svc = Service {
		network_mode: Some("service:db".to_string()),
		..Default::default()
	};
	let (ns, _) = resolve_network_mode("web", &svc, &file, "proj");
	assert_eq!(ns.unwrap().value.as_deref(), Some("custom-db"));
}

#[test]
fn network_mode_service_unknown_target_uses_raw_name() {
	let file = empty_file();
	let svc = Service {
		network_mode: Some("service:missing".to_string()),
		..Default::default()
	};
	let (ns, _) = resolve_network_mode("web", &svc, &file, "proj");
	assert_eq!(ns.unwrap().value.as_deref(), Some("missing"));
}

#[test]
fn resolve_network_mode_no_networks() {
	let svc = Service::default();
	let file = empty_file();
	let (ns, nets) = resolve_network_mode("web", &svc, &file, "proj");
	assert!(ns.is_none());
	assert!(nets.is_empty());
}

#[test]
fn build_per_network_options_seeds_service_name_alias() {
	// With no explicit config, the service name is still registered as an
	// alias so siblings can reach the service by name.
	let opts = build_per_network_options("web", None, None);
	assert_eq!(opts.aliases, vec!["web".to_string()]);
	assert!(opts.static_ips.is_empty());
}

#[test]
fn build_per_network_options_empty_service_name_adds_no_alias() {
	let opts = build_per_network_options("", None, None);
	assert!(opts.aliases.is_empty());
}

#[test]
fn build_per_network_options_with_aliases() {
	use crate::compose::types::ServiceNetworkConfig;
	let cfg = ServiceNetworkConfig {
		aliases: Some(vec!["api".to_string()]),
		..Default::default()
	};
	// The service name is prepended ahead of any explicit aliases.
	let opts = build_per_network_options("web", Some(&cfg), None);
	assert_eq!(opts.aliases, vec!["web".to_string(), "api".to_string()]);
}

#[test]
fn build_per_network_options_does_not_duplicate_service_name() {
	use crate::compose::types::ServiceNetworkConfig;
	let cfg = ServiceNetworkConfig {
		aliases: Some(vec!["web".to_string(), "api".to_string()]),
		..Default::default()
	};
	// An explicit alias equal to the service name is not duplicated.
	let opts = build_per_network_options("web", Some(&cfg), None);
	assert_eq!(opts.aliases, vec!["web".to_string(), "api".to_string()]);
}

#[test]
fn build_per_network_options_with_ipv4() {
	use crate::compose::types::ServiceNetworkConfig;
	let cfg = ServiceNetworkConfig {
		ipv4_address: Some("10.0.0.5".to_string()),
		..Default::default()
	};
	let opts = build_per_network_options("web", Some(&cfg), None);
	assert!(opts.static_ips.contains(&"10.0.0.5".to_string()));
}

#[test]
fn fallback_mac_applied_when_no_config() {
	let opts = build_per_network_options("web", None, Some("02:42:ac:11:00:02"));
	assert_eq!(opts.static_mac.as_deref(), Some("02:42:ac:11:00:02"));
}

#[test]
fn lease_range_ipv4_reserves_network_and_broadcast() {
	let lr = lease_range_from_cidr("172.28.5.0/24").unwrap();
	assert_eq!(lr.start_ip.as_deref(), Some("172.28.5.1"));
	assert_eq!(lr.end_ip.as_deref(), Some("172.28.5.254"));
}

#[test]
fn lease_range_ipv4_slash31_uses_both_addresses() {
	let lr = lease_range_from_cidr("10.0.0.0/31").unwrap();
	assert_eq!(lr.start_ip.as_deref(), Some("10.0.0.0"));
	assert_eq!(lr.end_ip.as_deref(), Some("10.0.0.1"));
}

#[test]
fn lease_range_ipv6_full_span() {
	let lr = lease_range_from_cidr("2001:db8::/120").unwrap();
	assert_eq!(lr.start_ip.as_deref(), Some("2001:db8::"));
	assert_eq!(lr.end_ip.as_deref(), Some("2001:db8::ff"));
}

#[test]
fn lease_range_invalid_cidr_is_none() {
	assert!(lease_range_from_cidr("not-a-cidr").is_none());
	assert!(lease_range_from_cidr("10.0.0.0/40").is_none());
}

#[test]
fn ipam_options_include_driver_and_options() {
	use crate::compose::types::IpamConfig;
	let ipam = IpamConfig {
		driver: Some("host-local".into()),
		options: [("foo".to_string(), "bar".to_string())].into(),
		..Default::default()
	};
	let opts = build_ipam_options(&ipam);
	assert_eq!(opts.get("driver").map(String::as_str), Some("host-local"));
	assert_eq!(opts.get("foo").map(String::as_str), Some("bar"));
}

#[test]
fn per_network_interface_name_forwarded() {
	use crate::compose::types::ServiceNetworkConfig;
	let cfg = ServiceNetworkConfig {
		interface_name: Some("eth1".into()),
		..Default::default()
	};
	let opts = build_per_network_options("web", Some(&cfg), None);
	assert_eq!(opts.interface_name.as_deref(), Some("eth1"));
}

#[test]
fn per_network_mac_takes_precedence_over_fallback() {
	use crate::compose::types::ServiceNetworkConfig;
	let cfg = ServiceNetworkConfig {
		mac_address: Some("aa:bb:cc:dd:ee:ff".to_string()),
		..Default::default()
	};
	let opts = build_per_network_options("web", Some(&cfg), Some("02:42:ac:11:00:03"));
	assert_eq!(opts.static_mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
}

#[test]
fn per_network_ipv6_and_link_local_become_static_ips() {
	use crate::compose::types::ServiceNetworkConfig;
	let cfg = ServiceNetworkConfig {
		ipv4_address: Some("10.0.0.5".to_string()),
		ipv6_address: Some("2001:db8::5".to_string()),
		link_local_ips: vec!["169.254.0.1".to_string()],
		..Default::default()
	};
	let opts = build_per_network_options("web", Some(&cfg), None);
	// All three address forms are folded into the static-IP list.
	assert!(opts.static_ips.contains(&"10.0.0.5".to_string()));
	assert!(opts.static_ips.contains(&"2001:db8::5".to_string()));
	assert!(opts.static_ips.contains(&"169.254.0.1".to_string()));
}

#[test]
fn per_network_priority_folds_into_driver_opts() {
	use crate::compose::types::ServiceNetworkConfig;
	let mut driver_opts = std::collections::HashMap::new();
	driver_opts.insert("mtu".to_string(), "1400".to_string());
	let cfg = ServiceNetworkConfig {
		priority: Some(100),
		driver_opts,
		..Default::default()
	};
	let opts = build_per_network_options("web", Some(&cfg), None);
	let opts = opts.driver_opts.expect("driver opts present");
	// Compose `priority` is consumed by Podman as a driver option, alongside the
	// explicit driver_opts.
	assert_eq!(opts.get("priority").map(String::as_str), Some("100"));
	assert_eq!(opts.get("mtu").map(String::as_str), Some("1400"));
}

#[test]
fn build_subnets_maps_pool_fields_and_ip_range() {
	use crate::compose::types::{IpamConfig, IpamPool};
	let ipam = IpamConfig {
		config: vec![IpamPool {
			subnet: Some("172.28.0.0/16".to_string()),
			gateway: Some("172.28.0.1".to_string()),
			ip_range: Some("172.28.5.0/24".to_string()),
			..Default::default()
		}],
		..Default::default()
	};
	let subnets = super::build_subnets(&ipam);
	assert_eq!(subnets.len(), 1);
	assert_eq!(subnets[0].subnet.as_deref(), Some("172.28.0.0/16"));
	assert_eq!(subnets[0].gateway.as_deref(), Some("172.28.0.1"));
	// The ip_range CIDR is translated into a Podman lease range.
	let lr = subnets[0]
		.lease_range
		.as_ref()
		.expect("lease range derived");
	assert_eq!(lr.start_ip.as_deref(), Some("172.28.5.1"));
	assert_eq!(lr.end_ip.as_deref(), Some("172.28.5.254"));
}

#[test]
fn build_subnets_without_ip_range_has_no_lease_range() {
	use crate::compose::types::{IpamConfig, IpamPool};
	let ipam = IpamConfig {
		config: vec![IpamPool {
			subnet: Some("10.0.0.0/24".to_string()),
			..Default::default()
		}],
		..Default::default()
	};
	let subnets = super::build_subnets(&ipam);
	assert!(subnets[0].lease_range.is_none());
}

#[test]
fn lease_range_ipv6_zero_prefix_spans_whole_space() {
	// A `/0` IPv6 range uses an all-zero mask: the start is `::` and the end is
	// the maximum address.
	let lr = lease_range_from_cidr("::/0").unwrap();
	assert_eq!(lr.start_ip.as_deref(), Some("::"));
	assert_eq!(
		lr.end_ip.as_deref(),
		Some("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff")
	);
}

#[test]
fn lease_range_ipv6_rejects_oversized_prefix() {
	assert!(lease_range_from_cidr("2001:db8::/129").is_none());
}

/// Docker isolates bridge networks from each other and podman does not, so a
/// podup project used to let one service reach unpublished ports on another
/// network. Measured on both engines; see `apply_default_isolation`.
#[test]
fn a_bridge_network_is_isolated_by_default() {
	let mut opts = HashMap::new();
	apply_default_isolation("bridge", &mut opts);
	assert_eq!(opts.get("isolate").map(String::as_str), Some("true"));
}

/// The option is netavark's bridge plugin. Setting it on a driver that does
/// not read it would be noise at best and a create failure at worst.
#[test]
fn only_bridge_networks_get_the_option() {
	for driver in ["macvlan", "ipvlan", "host"] {
		let mut opts = HashMap::new();
		apply_default_isolation(driver, &mut opts);
		assert!(opts.is_empty(), "{driver} should not be given isolate");
	}
}

/// The escape hatch for a project that deliberately spans networks. A default
/// that cannot be turned off is not a default, it is a policy.
#[test]
fn an_explicit_driver_opt_wins_in_both_directions() {
	let mut off = HashMap::from([("isolate".to_string(), "false".to_string())]);
	apply_default_isolation("bridge", &mut off);
	assert_eq!(off.get("isolate").map(String::as_str), Some("false"));

	let mut strict = HashMap::from([("isolate".to_string(), "strict".to_string())]);
	apply_default_isolation("bridge", &mut strict);
	assert_eq!(strict.get("isolate").map(String::as_str), Some("strict"));
}

/// Whatever else the compose file asked for has to survive.
#[test]
fn other_driver_opts_are_left_alone() {
	let mut opts = HashMap::from([("mtu".to_string(), "9000".to_string())]);
	apply_default_isolation("bridge", &mut opts);
	assert_eq!(opts.get("mtu").map(String::as_str), Some("9000"));
	assert_eq!(opts.get("isolate").map(String::as_str), Some("true"));
}
