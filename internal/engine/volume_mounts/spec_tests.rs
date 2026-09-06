use super::{
	extend_bind_opts_str, is_bind_source, map_selinux_option, parse_tmpfs_string,
	parse_volume_string, split_volume_spec,
};
use crate::compose::types::BindOptions;

#[test]
fn tmpfs_short_form_splits_path_from_options() {
	// Regression: the whole entry used to become the destination, so
	// `/multi:size=64m,…` mounted a tmpfs at a directory *named* that, with no
	// size cap, while the real path stayed untouched. Silent, exit 0.
	let m = parse_tmpfs_string("/multi:size=64m,mode=1777,noexec,nosuid,nodev");
	assert_eq!(m.destination, "/multi", "path must not carry the options");
	assert_eq!(
		m.options,
		vec!["size=64m", "mode=1777", "noexec", "nosuid", "nodev"],
		"every option must reach the engine"
	);
	assert_eq!(m.mount_type, "tmpfs");
	assert!(m.source.is_none(), "a tmpfs has no source");
}

#[test]
fn tmpfs_short_form_without_options_is_unchanged() {
	let m = parse_tmpfs_string("/plain");
	assert_eq!(m.destination, "/plain");
	assert!(m.options.is_empty(), "no colon means no options");
}

#[test]
fn tmpfs_short_form_trailing_colon_yields_no_options() {
	// Matches docker compose, measured: `/trail:` mounts /trail with defaults
	// rather than an empty-string option the engine would reject.
	let m = parse_tmpfs_string("/trail:");
	assert_eq!(m.destination, "/trail");
	assert!(m.options.is_empty());
}

#[test]
fn tmpfs_short_form_splits_on_the_first_colon_only() {
	// An option value may itself contain a colon; only the first separates
	// the path from the options.
	let m = parse_tmpfs_string("/x:size=1m,context=system_u:object_r:tmp_t:s0");
	assert_eq!(m.destination, "/x");
	assert_eq!(
		m.options,
		vec!["size=1m", "context=system_u:object_r:tmp_t:s0"]
	);
}

#[test]
fn colon_less_path_is_anonymous_volume_not_bind() {
	// `- /data` (no `src:dst`) is a single in-container target: an anonymous
	// volume, not a host bind of `/data`.
	let (mount, named) = parse_volume_string("/data").unwrap();
	assert!(mount.is_none(), "must not be a bind mount");
	let nv = named.expect("expected an anonymous named volume");
	assert_eq!(nv.name, "", "anonymous volume carries no name");
	assert_eq!(nv.dest, "/data");
}

#[test]
fn colon_less_relative_token_is_anonymous_volume() {
	// A bare token with no separator is still a single target, so it produces
	// an anonymous volume rather than being read as a host bind.
	let (mount, named) = parse_volume_string("cache").unwrap();
	assert!(mount.is_none());
	assert_eq!(named.unwrap().dest, "cache");
}

#[test]
fn explicit_pair_still_binds_host_path() {
	// An explicit `src:dst` with a host-path source is still a bind mount.
	let (mount, named) = parse_volume_string("/host:/data").unwrap();
	assert!(named.is_none());
	let m = mount.expect("expected a bind mount");
	assert_eq!(m.mount_type, "bind");
	assert_eq!(m.source.as_deref(), Some("/host"));
	assert_eq!(m.destination, "/data");
}

#[test]
fn selinux_shared_maps_to_lowercase_z() {
	assert_eq!(map_selinux_option("shared"), "z");
}

#[test]
fn selinux_private_maps_to_uppercase_z() {
	assert_eq!(map_selinux_option("private"), "Z");
}

#[test]
fn selinux_other_values_pass_through() {
	// A raw Podman option (or any unrecognised value) is forwarded verbatim.
	assert_eq!(map_selinux_option("z"), "z");
	assert_eq!(map_selinux_option("Z"), "Z");
	assert_eq!(map_selinux_option("custom"), "custom");
}

#[test]
fn extend_bind_opts_translates_selinux() {
	let bind = BindOptions {
		selinux: Some("private".into()),
		..Default::default()
	};
	let mut opts = Vec::new();
	extend_bind_opts_str(&mut opts, Some(&bind));
	assert!(opts.contains(&"Z".to_string()), "expected Z, got {opts:?}");
}

#[test]
fn windows_drive_source_is_a_bind_not_a_named_volume() {
	// `C:\data:/in/container`: the drive colon must not be read as the
	// src/dst separator, and the source must classify as a bind.
	assert_eq!(
		split_volume_spec(r"C:\data:/in/container"),
		(r"C:\data", "/in/container", "")
	);
	assert!(is_bind_source(r"C:\data"));
	assert!(is_bind_source("D:/forward/slash"));
}

#[test]
fn unix_volume_split_is_unchanged() {
	assert_eq!(split_volume_spec("vol:/data"), ("vol", "/data", ""));
	assert_eq!(split_volume_spec("./src:/dst:ro"), ("./src", "/dst", "ro"));
	assert_eq!(split_volume_spec("named"), ("named", "named", ""));
	assert!(!is_bind_source("named"));
	assert!(is_bind_source("/abs"));
	assert!(is_bind_source("./rel"));
	assert!(is_bind_source("~/home"));
}
