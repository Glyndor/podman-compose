use super::*;
use indexmap::IndexMap;

// ServiceNetworks::names

#[test]
fn service_networks_empty_has_no_names() {
	assert!(ServiceNetworks::Empty.names().is_empty());
}

#[test]
fn service_networks_list_returns_names() {
	let n = ServiceNetworks::List(vec!["front".into(), "back".into()]);
	assert_eq!(n.names(), vec!["front", "back"]);
}

#[test]
fn service_networks_map_returns_keys() {
	let mut m = IndexMap::new();
	m.insert("front".to_string(), None);
	assert_eq!(ServiceNetworks::Map(m).names(), vec!["front"]);
}

// ServiceNetworks::config_for

#[test]
fn config_for_list_returns_none() {
	let n = ServiceNetworks::List(vec!["front".into()]);
	assert!(n.config_for("front").is_none());
}

#[test]
fn config_for_map_with_none_config_returns_none() {
	let mut m = IndexMap::new();
	m.insert("front".to_string(), None::<ServiceNetworkConfig>);
	assert!(ServiceNetworks::Map(m).config_for("front").is_none());
}

#[test]
fn config_for_map_with_config_returns_it() {
	let cfg = ServiceNetworkConfig {
		ipv4_address: Some("10.0.0.2".into()),
		..Default::default()
	};
	let mut m = IndexMap::new();
	m.insert("front".to_string(), Some(cfg));
	let nets = ServiceNetworks::Map(m);
	let result = nets.config_for("front");
	assert_eq!(result.unwrap().ipv4_address.as_deref(), Some("10.0.0.2"));
}

#[test]
fn config_for_missing_key_returns_none() {
	let mut m = IndexMap::new();
	m.insert("front".to_string(), None::<ServiceNetworkConfig>);
	assert!(ServiceNetworks::Map(m).config_for("back").is_none());
}
