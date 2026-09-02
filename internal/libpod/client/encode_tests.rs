use super::{is_valid_object_name, urlencoded};

#[test]
fn valid_object_names_accepted() {
	assert!(is_valid_object_name("web"));
	assert!(is_valid_object_name("proj-web-1"));
	assert!(is_valid_object_name("a.b_c-1"));
	assert!(is_valid_object_name("0abc"));
}

#[test]
fn invalid_object_names_rejected() {
	assert!(!is_valid_object_name(""));
	assert!(!is_valid_object_name("-leading-dash"));
	assert!(!is_valid_object_name(".leading-dot"));
	assert!(!is_valid_object_name("has space"));
	assert!(!is_valid_object_name("has/slash"));
	assert!(!is_valid_object_name("tab\tname"));
	assert!(!is_valid_object_name("emoji😀"));
}

#[test]
fn unreserved_chars_pass_through() {
	assert_eq!(urlencoded("abc-XYZ_0.9~"), "abc-XYZ_0.9~");
}

#[test]
fn space_encoded() {
	assert_eq!(urlencoded("hello world"), "hello%20world");
}

#[test]
fn slash_encoded() {
	assert_eq!(urlencoded("a/b"), "a%2Fb");
}

#[test]
fn colon_encoded() {
	assert_eq!(urlencoded("myproj:v1"), "myproj%3Av1");
}

#[test]
fn empty_string() {
	assert_eq!(urlencoded(""), "");
}

#[test]
fn unicode_byte_encoded() {
	// '€' = 0xE2 0x82 0xAC in UTF-8
	assert_eq!(urlencoded("€"), "%E2%82%AC");
}

#[test]
fn container_name_typical() {
	assert_eq!(urlencoded("myproject-web"), "myproject-web");
}

#[test]
fn container_name_with_brackets() {
	assert_eq!(urlencoded("a[b]"), "a%5Bb%5D");
}
