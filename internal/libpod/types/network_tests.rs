use super::*;

#[test]
fn network_create_minimal() {
	let req = NetworkCreateRequest {
		name: "mynet".into(),
		dns_enabled: Some(true),
		..Default::default()
	};
	let v = serde_json::to_value(&req).unwrap();
	assert_eq!(v["name"], "mynet");
	assert_eq!(v["dns_enabled"], serde_json::json!(true));
	assert!(v.get("labels").is_none());
	assert!(v.get("subnets").is_none());
}

#[test]
fn network_create_skips_empty_labels() {
	let req = NetworkCreateRequest {
		name: "n".into(),
		..Default::default()
	};
	let v = serde_json::to_value(&req).unwrap();
	assert!(v.get("labels").is_none());
}

#[test]
fn subnet_with_gateway() {
	let s = Subnet {
		subnet: Some("10.89.0.0/24".into()),
		gateway: Some("10.89.0.1".into()),
		lease_range: None,
	};
	let v = serde_json::to_value(&s).unwrap();
	assert_eq!(v["subnet"], "10.89.0.0/24");
	assert_eq!(v["gateway"], "10.89.0.1");
}
