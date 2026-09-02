use super::build_mounts_all;
use crate::compose::types::{BindOptions, Service, VolumeMount, VolumeOptions, VolumeType};
use std::path::Path;

fn svc_with_volumes(vols: Vec<VolumeMount>) -> Service {
	Service {
		volumes: vols,
		..Default::default()
	}
}

#[test]
fn short_form_bind_passthrough() {
	let svc = svc_with_volumes(vec![VolumeMount::Short("./data:/app/data".into())]);
	let (mounts, named) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(mounts.len(), 1);
	assert!(named.is_empty());
	assert_eq!(mounts[0].mount_type, "bind");
	assert_eq!(mounts[0].destination, "/app/data");
}

#[test]
fn short_form_named_volume() {
	let svc = svc_with_volumes(vec![VolumeMount::Short("myvolume:/data".into())]);
	let (mounts, named) = build_mounts_all(&svc, Path::new("/base"));
	assert!(mounts.is_empty());
	assert_eq!(named.len(), 1);
	assert_eq!(named[0].name, "myvolume");
	assert_eq!(named[0].dest, "/data");
}

#[test]
fn long_form_bind_read_only() {
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Bind,
		source: Some("/host/path".into()),
		target: "/container/path".into(),
		read_only: Some(true),
		bind: None,
		volume: None,
		tmpfs: None,
		consistency: None,
	}]);
	let (mounts, _) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(mounts.len(), 1);
	assert_eq!(mounts[0].mount_type, "bind");
	assert!(mounts[0].options.contains(&"ro".to_string()));
	assert_eq!(mounts[0].destination, "/container/path");
}

#[test]
fn long_form_bind_with_propagation() {
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Bind,
		source: Some("/host".into()),
		target: "/cont".into(),
		read_only: Some(false),
		bind: Some(BindOptions {
			propagation: Some("rshared".into()),
			create_host_path: None,
			selinux: None,
		}),
		volume: None,
		tmpfs: None,
		consistency: None,
	}]);
	let (mounts, _) = build_mounts_all(&svc, Path::new("/base"));
	assert!(mounts[0].options.contains(&"rshared".to_string()));
}

#[test]
fn long_form_volume_nocopy() {
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Volume,
		source: Some("myvolume".into()),
		target: "/data".into(),
		read_only: None,
		bind: None,
		volume: Some(VolumeOptions {
			nocopy: Some(true),
			..Default::default()
		}),
		tmpfs: None,
		consistency: None,
	}]);
	let (mounts, named) = build_mounts_all(&svc, Path::new("/base"));
	assert!(mounts.is_empty());
	assert_eq!(named.len(), 1);
	assert_eq!(named[0].name, "myvolume");
	assert!(named[0].options.contains(&"nocopy".to_string()));
}

/// The mount-hardening trio reaches the engine from the long form, matching
/// what the short form's raw options have always done (#1160). `false` and
/// absent both mean "not hardened": only an explicit `true` emits the flag.
#[test]
fn long_form_volume_hardening_options_forwarded() {
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Volume,
		source: Some("myvolume".into()),
		target: "/data".into(),
		read_only: None,
		bind: None,
		volume: Some(VolumeOptions {
			noexec: Some(true),
			nosuid: Some(true),
			nodev: Some(false),
			..Default::default()
		}),
		tmpfs: None,
		consistency: None,
	}]);
	let (_, named) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(named.len(), 1);
	assert!(named[0].options.contains(&"noexec".to_string()));
	assert!(named[0].options.contains(&"nosuid".to_string()));
	assert!(
		!named[0].options.contains(&"nodev".to_string()),
		"an explicit false must not emit the flag"
	);
}

#[test]
fn long_form_volume_subpath_forwarded() {
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Volume,
		source: Some("myvolume".into()),
		target: "/data".into(),
		read_only: None,
		bind: None,
		volume: Some(VolumeOptions {
			subpath: Some("nested/dir".into()),
			..Default::default()
		}),
		tmpfs: None,
		consistency: None,
	}]);
	let (_, named) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(named.len(), 1);
	assert_eq!(named[0].sub_path.as_deref(), Some("nested/dir"));
}

#[test]
fn npipe_type_becomes_npipe_mount() {
	// A long-form `type: npipe` mount maps straight to an npipe OCI mount,
	// carrying its source/target with no extra options.
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Npipe,
		source: Some(r"\\.\pipe\docker_engine".into()),
		target: r"\\.\pipe\docker_engine".into(),
		read_only: None,
		bind: None,
		volume: None,
		tmpfs: None,
		consistency: None,
	}]);
	let (mounts, named) = build_mounts_all(&svc, Path::new("/base"));
	assert!(named.is_empty());
	assert_eq!(mounts.len(), 1);
	assert_eq!(mounts[0].mount_type, "npipe");
	assert_eq!(mounts[0].source.as_deref(), Some(r"\\.\pipe\docker_engine"));
	assert!(mounts[0].options.is_empty());
}

#[test]
fn cluster_type_becomes_cluster_mount() {
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Cluster,
		source: Some("my-cluster-vol".into()),
		target: "/data".into(),
		read_only: None,
		bind: None,
		volume: None,
		tmpfs: None,
		consistency: None,
	}]);
	let (mounts, _) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(mounts.len(), 1);
	assert_eq!(mounts[0].mount_type, "cluster");
	assert_eq!(mounts[0].destination, "/data");
	assert_eq!(mounts[0].source.as_deref(), Some("my-cluster-vol"));
}

#[test]
fn tmpfs_type_becomes_tmpfs_mount() {
	use crate::compose::types::TmpfsOptions;
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Tmpfs,
		source: None,
		target: "/tmp/cache".into(),
		read_only: None,
		bind: None,
		volume: None,
		tmpfs: Some(TmpfsOptions {
			size: Some(65536),
			mode: Some(0o700),
		}),
		consistency: None,
	}]);
	let (mounts, _) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(mounts.len(), 1);
	assert_eq!(mounts[0].mount_type, "tmpfs");
	assert_eq!(mounts[0].destination, "/tmp/cache");
	assert!(mounts[0].options.iter().any(|o| o.starts_with("size=")));
	assert!(mounts[0].options.iter().any(|o| o.starts_with("mode=")));
}

#[test]
fn create_host_path_creates_directory() {
	let dir = tempfile::tempdir().unwrap();
	let rel = "subdir/nested";
	let svc = svc_with_volumes(vec![VolumeMount::Long {
		volume_type: VolumeType::Bind,
		source: Some(rel.into()),
		target: "/cont".into(),
		read_only: None,
		bind: Some(BindOptions {
			propagation: None,
			create_host_path: Some(true),
			selinux: None,
		}),
		volume: None,
		tmpfs: None,
		consistency: None,
	}]);
	build_mounts_all(&svc, dir.path());
	assert!(dir.path().join(rel).exists());
}

#[test]
fn short_form_bind_creates_missing_host_path() {
	// A short-form bind whose relative source is missing must have its host
	// directory created (compose-spec implies create_host_path for short
	// syntax), anchored to the project base dir — not left to fail with a raw
	// podman statfs 500.
	let dir = tempfile::tempdir().unwrap();
	let rel = "missing-dir";
	let svc = svc_with_volumes(vec![VolumeMount::Short(format!("./{rel}:/app/data"))]);
	let (mounts, named) = build_mounts_all(&svc, dir.path());
	assert!(named.is_empty());
	assert_eq!(mounts.len(), 1);
	assert_eq!(mounts[0].mount_type, "bind");
	assert!(
		dir.path().join(rel).is_dir(),
		"short-form bind source directory should be created"
	);
}

#[test]
fn top_level_tmpfs_shorthand() {
	use crate::compose::types::StringOrList;
	let svc = Service {
		tmpfs: StringOrList::List(vec!["/tmp".into(), "/run".into()]),
		..Default::default()
	};
	let (mounts, _) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(mounts.len(), 2);
	assert_eq!(mounts[0].mount_type, "tmpfs");
	assert_eq!(mounts[0].destination, "/tmp");
	assert_eq!(mounts[1].destination, "/run");
}

#[test]
fn top_level_tmpfs_single_string() {
	use crate::compose::types::StringOrList;
	let svc = Service {
		tmpfs: StringOrList::Single("/tmp".into()),
		..Default::default()
	};
	let (mounts, _) = build_mounts_all(&svc, Path::new("/base"));
	assert_eq!(mounts.len(), 1);
	assert_eq!(mounts[0].mount_type, "tmpfs");
	assert_eq!(mounts[0].destination, "/tmp");
}
