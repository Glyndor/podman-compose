use super::*;

// IncludeConfig::paths

#[test]
fn include_config_path_returns_single() {
	let c = IncludeConfig::Path("base.yml".into());
	assert_eq!(c.paths(), vec!["base.yml"]);
}

#[test]
fn include_config_long_returns_list() {
	let c = IncludeConfig::Long {
		path: super::super::StringOrList::List(vec!["a.yml".into(), "b.yml".into()]),
		env_file: None,
		project_directory: None,
	};
	assert_eq!(c.paths(), vec!["a.yml", "b.yml"]);
}

// ExtendsConfig

#[test]
fn extends_service_short_form() {
	let e = ExtendsConfig::Service("base".into());
	assert_eq!(e.service(), "base");
	assert!(e.file().is_none());
}

#[test]
fn extends_config_long_form() {
	let e = ExtendsConfig::Long {
		service: "base".into(),
		file: Some("base.yml".into()),
	};
	assert_eq!(e.service(), "base");
	assert_eq!(e.file(), Some("base.yml"));
}

// BuildConfig accessor methods

#[test]
fn build_config_context_string() {
	let b = BuildConfig::Context("./app".into());
	assert_eq!(b.context(), "./app");
	assert!(b.dockerfile().is_none());
	assert!(!b.no_cache());
	assert!(!b.pull());
}

#[test]
fn build_config_long_form_context() {
	let b = BuildConfig::Config {
		context: Some("./app".into()),
		dockerfile: Some("Dockerfile.prod".into()),
		dockerfile_inline: None,
		args: EnvVars::Empty,
		target: Some("release".into()),
		cache_from: vec![],
		cache_to: vec![],
		labels: Labels::Empty,
		shm_size: None,
		network: None,
		platforms: vec![],
		additional_contexts: Default::default(),
		no_cache: Some(true),
		pull: None,
		extra_hosts: vec![],
		tags: vec![],
		privileged: None,
		ssh: vec![],
		secrets: vec![],
		ulimits: Default::default(),
		isolation: None,
		entitlements: vec![],
		provenance: None,
		sbom: None,
	};
	assert_eq!(b.context(), "./app");
	assert_eq!(b.dockerfile(), Some("Dockerfile.prod"));
	assert_eq!(b.target(), Some("release"));
	assert!(b.no_cache());
}

#[test]
fn build_with_only_dockerfile_inline_defaults_context_to_dot() {
	// Compose Spec (v2.22+): `build:` may carry only `dockerfile_inline:` with
	// no `context:`, so the context then defaults to the project directory `.`.
	let b: BuildConfig = serde_yaml::from_str("dockerfile_inline: |\n  FROM alpine\n").unwrap();
	assert!(matches!(b, BuildConfig::Config { .. }));
	assert_eq!(b.context(), ".");
	assert_eq!(b.dockerfile_inline(), Some("FROM alpine\n"));
}

// --- BuildConfig::Context short-form: every accessor returns its empty default

#[test]
fn build_config_context_accessors_are_empty_defaults() {
	let b = BuildConfig::Context("./app".into());
	assert!(matches!(b.args(), EnvVars::Empty));
	assert!(b.target().is_none());
	assert!(b.shm_size().is_none());
	assert!(b.dockerfile_inline().is_none());
	assert!(b.extra_hosts().is_empty());
	assert!(b.tags().is_empty());
	assert!(b.cache_from().is_empty());
	assert!(b.cache_to().is_empty());
	assert!(b.ssh().is_empty());
	assert!(b.secrets().is_empty());
	assert!(b.additional_contexts().is_empty());
}

// --- BuildConfig::Config long-form: accessors surface the parsed values

#[test]
fn build_config_long_form_lists_and_scalars() {
	let yaml = "\
context: ./svc
dockerfile_inline: |
  FROM scratch
args:
  KEY: value
shm_size: 128mb
cache_from:
  - type=registry,ref=example.com/cache
cache_to:
  - type=local,dest=/tmp/c
extra_hosts:
  - host.example:10.0.0.1
tags:
  - example.com/app:1.0
  - example.com/app:latest
ssh:
  - default
secrets:
  - db_password
additional_contexts:
  base: docker-image://alpine:3
";
	let b: BuildConfig = serde_yaml::from_str(yaml).unwrap();
	assert_eq!(b.context(), "./svc");
	assert_eq!(b.dockerfile_inline(), Some("FROM scratch\n"));
	assert!(matches!(b.args(), EnvVars::Map(_)));
	assert_eq!(b.shm_size(), Some("128mb"));
	assert_eq!(b.cache_from(), &["type=registry,ref=example.com/cache"]);
	assert_eq!(b.cache_to(), &["type=local,dest=/tmp/c"]);
	assert_eq!(b.extra_hosts(), &["host.example:10.0.0.1"]);
	assert_eq!(b.tags(), &["example.com/app:1.0", "example.com/app:latest"]);
	assert_eq!(b.ssh(), &["default"]);
	assert_eq!(b.secrets(), &["db_password"]);
	let extra = b.additional_contexts();
	assert_eq!(extra.len(), 1);
	assert_eq!(
		extra[0],
		("base".to_string(), "docker-image://alpine:3".to_string())
	);
}
