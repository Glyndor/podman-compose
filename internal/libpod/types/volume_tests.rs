use super::VolumeCreateOptions;

#[test]
fn driver_opts_serialize_as_options_not_driver_opts() {
	let mut opts = VolumeCreateOptions {
		driver: Some("local".to_string()),
		..Default::default()
	};
	opts.driver_opts
		.insert("type".to_string(), "nfs".to_string());
	let v = serde_json::to_value(&opts).unwrap();
	// Podman's VolumeCreateOptions has no json tag on the options map, so the
	// wire key must be `Options`; `driver_opts` would be silently dropped.
	assert!(v.get("Options").is_some(), "expected Options key: {v}");
	assert!(v.get("driver_opts").is_none(), "stale driver_opts key: {v}");
	assert_eq!(v["Options"]["type"], "nfs");
}
