use super::mount_source_name;
use crate::compose::types::VolumeMount;

#[test]
fn named_volume_short_form_has_source() {
	assert_eq!(
		mount_source_name(&VolumeMount::Short("data:/var/lib".into())),
		Some("data".to_string())
	);
}

#[test]
fn bind_and_anonymous_have_no_source() {
	assert_eq!(
		mount_source_name(&VolumeMount::Short("./host:/c".into())),
		None
	);
	assert_eq!(
		mount_source_name(&VolumeMount::Short("/abs:/c".into())),
		None
	);
	assert_eq!(mount_source_name(&VolumeMount::Short("/data".into())), None);
}
