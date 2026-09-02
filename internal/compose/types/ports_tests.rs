use super::StringOrU16;

#[test]
fn string_variant_returns_string() {
	assert_eq!(
		StringOrU16::String("8080-8090".into()).as_str_val(),
		"8080-8090"
	);
}

#[test]
fn number_variant_returns_string_representation() {
	assert_eq!(StringOrU16::Number(443).as_str_val(), "443");
}
