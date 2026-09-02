use super::*;

// TmpfsOptions::mode octal parsing

#[test]
fn tmpfs_mode_octal_string_is_parsed_as_octal() {
	// A leading-zero literal reaches us as a string and must parse as octal
	// (0700 → 448 permission bits) instead of failing opaquely.
	let opts: TmpfsOptions = serde_yaml::from_str("mode: \"0700\"\n").unwrap();
	assert_eq!(opts.mode, Some(0o700));
	// An explicit 0o prefix in a string also works.
	let opts: TmpfsOptions = serde_yaml::from_str("mode: \"0o755\"\n").unwrap();
	assert_eq!(opts.mode, Some(0o755));
}

#[test]
fn tmpfs_mode_octal_yaml_literal_is_preserved_as_bits() {
	// A YAML `0o700` scalar is decoded to 448 by the parser; we keep those
	// actual permission bits so the renderer's octal format round-trips.
	let opts: TmpfsOptions = serde_yaml::from_str("mode: 0o700\n").unwrap();
	assert_eq!(opts.mode, Some(0o700));
}

#[test]
fn tmpfs_mode_invalid_octal_is_clear_error() {
	// A non-octal string is rejected with a clear error, not silently coerced.
	let err = serde_yaml::from_str::<TmpfsOptions>("mode: \"0o9\"\n").unwrap_err();
	assert!(err.to_string().contains("octal notation"), "got: {err}");
}

#[test]
fn tmpfs_mode_bare_decimal_is_interpreted_as_octal() {
	// A bare `700` is the octal file-mode the user typed, not a decimal value:
	// it must yield the same permission bits as `0700`/`0o700` (issue #917)
	// instead of being octal-encoded a second time at render time.
	let opts: TmpfsOptions = serde_yaml::from_str("mode: 700\n").unwrap();
	assert_eq!(opts.mode, Some(0o700));
	let opts: TmpfsOptions = serde_yaml::from_str("mode: 644\n").unwrap();
	assert_eq!(opts.mode, Some(0o644));
}

#[test]
fn int_mode_bits_treats_decoded_0o_literals_as_bits() {
	// A value carrying an 8/9 digit can only be a `0o` literal serde_yaml
	// already decoded (`0o755` → 493), so it is taken as the bits verbatim;
	// a value of valid octal digits is read as the octal the user typed.
	assert_eq!(int_mode_bits(700), 0o700);
	assert_eq!(int_mode_bits(0o755), 0o755);
	assert_eq!(int_mode_bits(0o700), 0o700);
}

// VolumeMount::target

#[test]
fn volume_mount_short_two_parts_returns_second() {
	let m = VolumeMount::Short("./data:/app/data".to_string());
	assert_eq!(m.target(), "/app/data");
}

#[test]
fn volume_mount_short_three_parts_returns_second() {
	let m = VolumeMount::Short("./data:/app/data:ro".to_string());
	assert_eq!(m.target(), "/app/data");
}

#[test]
fn volume_mount_short_no_colon_returns_whole_string() {
	let m = VolumeMount::Short("/app/data".to_string());
	assert_eq!(m.target(), "/app/data");
}

#[test]
fn volume_mount_long_returns_target_field() {
	let m = VolumeMount::Long {
		volume_type: VolumeType::Bind,
		source: Some("/host/path".to_string()),
		target: "/container/path".to_string(),
		read_only: None,
		bind: None,
		volume: None,
		tmpfs: None,
		consistency: None,
	};
	assert_eq!(m.target(), "/container/path");
}

// ServiceConfigRef

#[test]
fn config_ref_short_source() {
	let r = ServiceConfigRef::Short("my-config".to_string());
	assert_eq!(r.source(), "my-config");
	assert!(r.target().is_none());
}

#[test]
fn config_ref_long_source_and_target() {
	let r = ServiceConfigRef::Long {
		source: "my-config".to_string(),
		target: Some("/run/configs/my-config".to_string()),
		uid: None,
		gid: None,
		mode: None,
	};
	assert_eq!(r.source(), "my-config");
	assert_eq!(r.target(), Some("/run/configs/my-config"));
}

#[test]
fn config_ref_long_no_target() {
	let r = ServiceConfigRef::Long {
		source: "my-config".to_string(),
		target: None,
		uid: None,
		gid: None,
		mode: None,
	};
	assert!(r.target().is_none());
}

// ServiceSecretRef

#[test]
fn secret_ref_short_source() {
	let r = ServiceSecretRef::Short("my-secret".to_string());
	assert_eq!(r.source(), "my-secret");
	assert!(r.target().is_none());
}

#[test]
fn secret_ref_long_source_and_target() {
	let r = ServiceSecretRef::Long {
		source: "my-secret".to_string(),
		target: Some("/run/secrets/my-secret".to_string()),
		uid: None,
		gid: None,
		mode: None,
	};
	assert_eq!(r.source(), "my-secret");
	assert_eq!(r.target(), Some("/run/secrets/my-secret"));
}

#[test]
fn secret_ref_long_no_target() {
	let r = ServiceSecretRef::Long {
		source: "my-secret".to_string(),
		target: None,
		uid: None,
		gid: None,
		mode: None,
	};
	assert_eq!(r.source(), "my-secret");
	assert!(r.target().is_none());
}
