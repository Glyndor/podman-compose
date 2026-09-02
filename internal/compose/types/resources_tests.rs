use super::*;

fn rate(v: serde_yaml::Value) -> BlkioRateDevice {
	BlkioRateDevice {
		path: "/dev/sda".into(),
		rate: v,
	}
}

#[test]
fn rate_value_parses_number_and_size_string() {
	assert_eq!(rate(serde_yaml::Value::from(1048576)).rate_value(), 1048576);
	assert_eq!(rate(serde_yaml::Value::from("1mb")).rate_value(), 1_048_576);
}

#[test]
fn rate_value_coerces_invalid_inputs_to_zero() {
	// An unparseable size string, and a non-number/non-string node, both fall
	// back to 0 rather than erroring or panicking.
	assert_eq!(rate(serde_yaml::Value::from("not-a-size")).rate_value(), 0);
	assert_eq!(rate(serde_yaml::Value::Bool(true)).rate_value(), 0);
	assert_eq!(rate(serde_yaml::Value::Null).rate_value(), 0);
}

#[test]
fn gpu_to_count_covers_every_variant() {
	assert_eq!(GpuSpec::Named("all".into()).to_count(), -1);
	assert_eq!(GpuSpec::Count(2).to_count(), 2);
	// The device-list form is treated as "all" (-1); previously untested.
	assert_eq!(GpuSpec::Devices(vec![]).to_count(), -1);
}
