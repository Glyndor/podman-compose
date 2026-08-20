//! Build, include, and extends configuration types.
//!
//! [`BuildConfig`] represents the `build:` key — either a bare context string
//! or a full long-form config. [`IncludeConfig`] and [`ExtendsConfig`] handle
//! the `include:` and `extends:` top-level / per-service directives respectively.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{EnvVars, Labels, StringOrList, UlimitConfig};

/// Compose `include:` directive — either a bare file path or a long-form block.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum IncludeConfig {
	/// Short form: a single Compose file path to include.
	Path(String),
	/// Long form: one or more paths with optional env-file and project directory overrides.
	Long {
		/// One or more Compose file paths to include.
		path: StringOrList,
		/// Env file(s) used to interpolate the included files.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		env_file: Option<StringOrList>,
		/// Base directory for resolving relative paths in the included files.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		project_directory: Option<String>,
	},
}

impl IncludeConfig {
	/// Returns the included Compose file paths.
	pub fn paths(&self) -> Vec<String> {
		match self {
			IncludeConfig::Path(p) => vec![p.clone()],
			IncludeConfig::Long { path, .. } => path.to_list(),
		}
	}
}

/// Compose `extends:` directive — either a bare service name or a long-form `{service, file}` block.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ExtendsConfig {
	/// Short form: the name of a service in the current file to extend.
	Service(String),
	/// Long form: a service name plus the file it is defined in.
	Long {
		/// Name of the service to extend.
		service: String,
		/// File the extended service is defined in; the current file if absent.
		#[serde(skip_serializing_if = "Option::is_none")]
		file: Option<String>,
	},
}

impl ExtendsConfig {
	/// Returns the name of the service being extended.
	pub fn service(&self) -> &str {
		match self {
			ExtendsConfig::Service(s) => s,
			ExtendsConfig::Long { service, .. } => service,
		}
	}

	/// Returns the file the extended service is defined in, if specified.
	pub fn file(&self) -> Option<&str> {
		match self {
			ExtendsConfig::Service(_) => None,
			ExtendsConfig::Long { file, .. } => file.as_deref(),
		}
	}
}

/// Compose `build:` directive — either a bare context path or a full build-config block.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum BuildConfig {
	/// Short form: a bare build context path.
	Context(String),
	/// Long form: the full set of build options.
	Config {
		/// Build context path; defaults to the project directory `.` if absent.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		context: Option<String>,
		/// Path to the Dockerfile relative to the context.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		dockerfile: Option<String>,
		/// Inline Dockerfile contents, used in place of a Dockerfile path.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		dockerfile_inline: Option<String>,
		/// Build-time arguments passed to the builder.
		#[serde(default)]
		args: EnvVars,
		/// Target build stage to build in a multi-stage Dockerfile.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		target: Option<String>,
		/// Sources to consider for build cache resolution.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		cache_from: Vec<String>,
		/// Export locations for the build cache.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		cache_to: Vec<String>,
		/// Labels applied to the resulting image.
		#[serde(default)]
		labels: Labels,
		/// Size of `/dev/shm` available to the build.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		shm_size: Option<String>,
		/// Network mode containers use during the build.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		network: Option<String>,
		/// Target platforms to build the image for.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		platforms: Vec<String>,
		/// Named additional build contexts (`name -> source`).
		#[serde(default, skip_serializing_if = "HashMap::is_empty")]
		additional_contexts: HashMap<String, String>,
		/// Whether to build without using any cache.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		no_cache: Option<bool>,
		/// Whether to always attempt to pull newer versions of base images.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		pull: Option<bool>,
		/// Extra `host:ip` mappings added to the build container's `/etc/hosts`.
		#[serde(
			default,
			deserialize_with = "super::primitives::deserialize_extra_hosts",
			skip_serializing_if = "Vec::is_empty"
		)]
		extra_hosts: Vec<String>,
		/// Image references to tag the built image with.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		tags: Vec<String>,
		/// Whether to run the build with elevated privileges.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		privileged: Option<bool>,
		/// SSH agent sockets or keys exposed to the build.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		ssh: Vec<String>,
		/// Names of top-level secrets exposed to the build.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		secrets: Vec<String>,
		/// Resource limits applied to the build container.
		#[serde(default, skip_serializing_if = "IndexMap::is_empty")]
		ulimits: IndexMap<String, UlimitConfig>,
		/// Build isolation technology to use.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		isolation: Option<String>,
		/// Extra privileged entitlements granted to the build.
		#[serde(default, skip_serializing_if = "Vec::is_empty")]
		entitlements: Vec<String>,
		/// Provenance attestation setting for the build.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		provenance: Option<serde_yaml::Value>,
		/// Whether to generate an SBOM attestation for the build.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		sbom: Option<bool>,
	},
}

impl BuildConfig {
	/// Build context path.
	///
	/// In the long form, `context` is optional per the Compose Spec (v2.22+):
	/// a `build:` block carrying only `dockerfile_inline:` has no context and
	/// defaults to the project directory `.`.
	pub fn context(&self) -> &str {
		match self {
			BuildConfig::Context(ctx) => ctx,
			BuildConfig::Config { context, .. } => context.as_deref().unwrap_or("."),
		}
	}

	/// Returns the Dockerfile path, if set.
	pub fn dockerfile(&self) -> Option<&str> {
		match self {
			BuildConfig::Context(_) => None,
			BuildConfig::Config { dockerfile, .. } => dockerfile.as_deref(),
		}
	}

	/// Returns the build arguments.
	pub fn args(&self) -> EnvVars {
		match self {
			BuildConfig::Context(_) => EnvVars::Empty,
			BuildConfig::Config { args, .. } => args.clone(),
		}
	}

	/// Returns the target build stage, if set.
	pub fn target(&self) -> Option<&str> {
		match self {
			BuildConfig::Context(_) => None,
			BuildConfig::Config { target, .. } => target.as_deref(),
		}
	}

	/// Returns whether the build should ignore the cache.
	pub fn no_cache(&self) -> bool {
		match self {
			BuildConfig::Context(_) => false,
			BuildConfig::Config { no_cache, .. } => no_cache.unwrap_or(false),
		}
	}

	/// Returns whether the build should always pull newer base images.
	pub fn pull(&self) -> bool {
		match self {
			BuildConfig::Context(_) => false,
			BuildConfig::Config { pull, .. } => pull.unwrap_or(false),
		}
	}

	/// Returns the `/dev/shm` size for the build, if set.
	pub fn shm_size(&self) -> Option<&str> {
		match self {
			BuildConfig::Context(_) => None,
			BuildConfig::Config { shm_size, .. } => shm_size.as_deref(),
		}
	}

	/// Returns the extra `host:ip` mappings for the build.
	pub fn extra_hosts(&self) -> &[String] {
		match self {
			BuildConfig::Context(_) => &[],
			BuildConfig::Config { extra_hosts, .. } => extra_hosts,
		}
	}

	/// Returns the image tags to apply to the built image.
	pub fn tags(&self) -> &[String] {
		match self {
			BuildConfig::Context(_) => &[],
			BuildConfig::Config { tags, .. } => tags,
		}
	}

	/// Returns the cache sources for the build.
	pub fn cache_from(&self) -> &[String] {
		match self {
			BuildConfig::Context(_) => &[],
			BuildConfig::Config { cache_from, .. } => cache_from,
		}
	}

	/// Returns the per-build ulimits.
	pub fn ulimits(&self) -> &IndexMap<String, UlimitConfig> {
		static EMPTY: std::sync::LazyLock<IndexMap<String, UlimitConfig>> =
			std::sync::LazyLock::new(IndexMap::new);
		match self {
			BuildConfig::Context(_) => &EMPTY,
			BuildConfig::Config { ulimits, .. } => ulimits,
		}
	}

	/// Returns the inline Dockerfile contents, if set.
	pub fn dockerfile_inline(&self) -> Option<&str> {
		match self {
			BuildConfig::Context(_) => None,
			BuildConfig::Config {
				dockerfile_inline, ..
			} => dockerfile_inline.as_deref(),
		}
	}

	/// `build.cache_to` — cache export targets (image references).
	pub fn cache_to(&self) -> &[String] {
		match self {
			BuildConfig::Context(_) => &[],
			BuildConfig::Config { cache_to, .. } => cache_to,
		}
	}

	/// `build.ssh` — SSH agent sockets/keys for the build.
	pub fn ssh(&self) -> &[String] {
		match self {
			BuildConfig::Context(_) => &[],
			BuildConfig::Config { ssh, .. } => ssh,
		}
	}

	/// `build.secrets` — names of top-level secrets exposed to the build.
	pub fn secrets(&self) -> &[String] {
		match self {
			BuildConfig::Context(_) => &[],
			BuildConfig::Config { secrets, .. } => secrets,
		}
	}

	/// `build.additional_contexts` — named extra build contexts (`name -> source`).
	pub fn additional_contexts(&self) -> Vec<(String, String)> {
		match self {
			BuildConfig::Context(_) => Vec::new(),
			BuildConfig::Config {
				additional_contexts,
				..
			} => additional_contexts
				.iter()
				.map(|(k, v)| (k.clone(), v.clone()))
				.collect(),
		}
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
		// no `context:` — the context then defaults to the project directory `.`.
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
}
