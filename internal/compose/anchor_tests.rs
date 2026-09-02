use super::*;
use crate::compose::types::Service;
use std::path::Path;

fn dir() -> &'static Path {
	Path::new("/proj/sub")
}

#[test]
#[cfg(unix)]
fn anchors_relative_build_context() {
	let mut svc = Service {
		build: Some(BuildConfig::Context("./app".into())),
		..Default::default()
	};
	anchor_service(&mut svc, dir());
	match svc.build.unwrap() {
		BuildConfig::Context(c) => assert_eq!(c, "/proj/sub/./app"),
		_ => panic!("expected context"),
	}
}

#[test]
fn leaves_url_build_context() {
	let mut svc = Service {
		build: Some(BuildConfig::Context("https://github.com/x/y.git".into())),
		..Default::default()
	};
	anchor_service(&mut svc, dir());
	match svc.build.unwrap() {
		BuildConfig::Context(c) => assert_eq!(c, "https://github.com/x/y.git"),
		_ => panic!("expected context"),
	}
}

#[test]
#[cfg(unix)]
fn anchors_env_file_list() {
	let mut svc = Service {
		env_file: EnvFile::List(vec![
			EnvFileEntry::Path("./a.env".into()),
			EnvFileEntry::Path("/abs/b.env".into()),
		]),
		..Default::default()
	};
	anchor_service(&mut svc, dir());
	let EnvFile::List(list) = &svc.env_file else {
		panic!("expected list");
	};
	assert_eq!(list[0].path(), "/proj/sub/./a.env");
	assert_eq!(list[1].path(), "/abs/b.env");
}

#[test]
#[cfg(unix)]
fn anchors_relative_short_bind_only() {
	let mut svc = Service {
		volumes: vec![
			VolumeMount::Short("./data:/data:ro".into()),
			VolumeMount::Short("named:/v".into()),
			VolumeMount::Short("/abs:/a".into()),
		],
		..Default::default()
	};
	anchor_service(&mut svc, dir());
	let got: Vec<_> = svc
		.volumes
		.iter()
		.map(|v| match v {
			VolumeMount::Short(s) => s.clone(),
			_ => unreachable!(),
		})
		.collect();
	assert_eq!(got[0], "/proj/sub/./data:/data:ro");
	assert_eq!(got[1], "named:/v");
	assert_eq!(got[2], "/abs:/a");
}

#[test]
fn leaves_tilde_paths() {
	let mut svc = Service {
		env_file: EnvFile::Single(EnvFileEntry::Path("~/x.env".into())),
		..Default::default()
	};
	anchor_service(&mut svc, dir());
	let EnvFile::Single(e) = &svc.env_file else {
		panic!("expected single");
	};
	assert_eq!(e.path(), "~/x.env");
}

#[test]
#[cfg(unix)]
fn anchors_long_form_build_context_default() {
	// A long-form build with no explicit `context:` defaults to `.`; that
	// default is anchored to the imported file's directory.
	let mut svc: Service =
		serde_yaml::from_str("build:\n  dockerfile_inline: |\n    FROM alpine\n").unwrap();
	anchor_service(&mut svc, dir());
	match svc.build.unwrap() {
		BuildConfig::Config { context, .. } => {
			assert_eq!(context.as_deref(), Some("/proj/sub/."));
		}
		_ => panic!("expected long-form config"),
	}
}

#[test]
#[cfg(unix)]
fn anchors_long_form_relative_bind_source() {
	// A long-form bind mount with a relative `./` source is anchored; a named
	// volume is left untouched.
	let mut svc: Service = serde_yaml::from_str(
		"volumes:\n  - type: bind\n    source: ./cfg\n    target: /cfg\n  \
		 - type: volume\n    source: named\n    target: /v\n",
	)
	.unwrap();
	anchor_service(&mut svc, dir());
	let VolumeMount::Long { source, .. } = &svc.volumes[0] else {
		panic!("expected long bind");
	};
	assert_eq!(source.as_deref(), Some("/proj/sub/./cfg"));
	// The named volume keeps its bare name (not a bind, not anchored).
	let VolumeMount::Long { source, .. } = &svc.volumes[1] else {
		panic!("expected long volume");
	};
	assert_eq!(source.as_deref(), Some("named"));
}

#[test]
#[cfg(unix)]
fn anchors_config_form_env_entry() {
	// The long-form (`{ path, required }`) env_file entry has its path anchored.
	let mut svc = Service {
		env_file: EnvFile::Single(EnvFileEntry::Config {
			path: "./svc.env".into(),
			required: Some(false),
			format: None,
		}),
		..Default::default()
	};
	anchor_service(&mut svc, dir());
	let EnvFile::Single(e) = &svc.env_file else {
		panic!("expected single");
	};
	assert_eq!(e.path(), "/proj/sub/./svc.env");
}

#[test]
#[cfg(unix)]
fn anchors_secret_and_config_files() {
	use crate::compose::types::{ConfigConfig, SecretConfig};
	let mut file = ComposeFile::default();
	file.secrets.insert(
		"tok".into(),
		SecretConfig {
			file: Some("./secret.txt".into()),
			..Default::default()
		},
	);
	// An absolute secret file is left as-is.
	file.secrets.insert(
		"abs".into(),
		SecretConfig {
			file: Some("/etc/abs.txt".into()),
			..Default::default()
		},
	);
	file.configs.insert(
		"cfg".into(),
		ConfigConfig {
			file: Some("./conf.yml".into()),
			..Default::default()
		},
	);
	anchor_compose_file(&mut file, dir());
	assert_eq!(
		file.secrets["tok"].file.as_deref(),
		Some("/proj/sub/./secret.txt")
	);
	assert_eq!(file.secrets["abs"].file.as_deref(), Some("/etc/abs.txt"));
	assert_eq!(
		file.configs["cfg"].file.as_deref(),
		Some("/proj/sub/./conf.yml")
	);
}
