use super::staging_base;

#[test]
fn staging_base_is_a_directory() {
	let base = staging_base().expect("staging base");
	assert!(base.is_dir());
	assert!(base.ends_with("podup"));
}
