//! Docker Compose file type definitions.

pub mod build;
pub mod deploy;
pub mod develop;
pub mod env;
pub mod lifecycle;
pub mod network;
pub mod ports;
pub mod primitives;
pub mod resources;
pub mod service;
pub mod volume;

pub use build::*;
pub use deploy::*;
pub use develop::*;
pub use env::*;
pub use lifecycle::*;
pub use network::*;
pub use ports::*;
pub use primitives::*;
pub use resources::*;
pub use service::*;
pub use volume::*;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level `secrets:` entry: defines a named secret available to services.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct SecretConfig {
	/// Host file supplying the secret value; mutually exclusive with `content` and `environment`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub file: Option<String>,
	/// When true, the secret must already exist in the engine; no value source is provided here.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub external: Option<bool>,
	/// Engine-side name; overrides the map key, and names the pre-existing secret when `external`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Inline secret value; mutually exclusive with `file` and `environment`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub content: Option<String>,
	/// Name of an environment variable supplying the value; mutually exclusive with `file` and `content`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub environment: Option<String>,
	/// Secret driver name (Swarm-style); parsed for fidelity, not honored by podman.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,
	/// Options passed to `driver`.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub driver_opts: HashMap<String, String>,
	/// Labels attached to the secret.
	#[serde(default)]
	pub labels: Labels,
	/// Driver used to render the secret as a template before delivery.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub template_driver: Option<String>,
}

/// Top-level `configs:` entry: defines a named config available to services.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct ConfigConfig {
	/// Host file supplying the config value; mutually exclusive with `content` and `environment`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub file: Option<String>,
	/// When true, the config must already exist in the engine; no value source is provided here.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub external: Option<bool>,
	/// Engine-side name; overrides the map key, and names the pre-existing config when `external`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Inline config value; mutually exclusive with `file` and `environment`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub content: Option<String>,
	/// Name of an environment variable supplying the value; mutually exclusive with `file` and `content`.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub environment: Option<String>,
	/// Labels attached to the config.
	#[serde(default)]
	pub labels: Labels,
	/// Config driver name (Swarm-style); parsed for fidelity, not honored by podman.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,
	/// Options passed to `driver`.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub driver_opts: HashMap<String, String>,
	/// Driver used to render the config as a template before delivery.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub template_driver: Option<String>,
}

/// Top-level `models:` entry (Compose v2.38) declares an AI model the
/// project depends on. podup runs no model runner, so the diagnostics pass
/// reports any declared model as not honored; the fields are parsed for
/// fidelity and round-tripped by `config`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct ModelConfig {
	/// OCI reference of the model artifact to pull.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub model: Option<String>,
	/// Engine-side name; overrides the map key.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Maximum context window, in tokens.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub context_size: Option<u64>,
	/// Raw flags forwarded to the model runner's inference engine.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub runtime_flags: Vec<String>,
	/// Extra variables injected into the model runtime environment.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub model_variables: HashMap<String, String>,
	/// Forward-compatible keys captured so a typo is surfaced rather than dropped.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub unknown: IndexMap<String, serde_yaml::Value>,
}

/// Root deserialization target for a `docker-compose.yml` file.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct ComposeFile {
	/// Top-level `version:`. Deprecated by the Compose Spec; parsed and round-tripped but otherwise ignored.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub version: Option<String>,
	/// Project name; overrides the directory-derived default.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Other Compose files merged into this project before processing.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub include: Vec<IncludeConfig>,
	/// Service definitions, keyed by service name.
	#[serde(default)]
	pub services: IndexMap<String, Service>,
	/// Named volumes; a `None` value is a default-configured volume (`name:` with no body).
	#[serde(default)]
	pub volumes: IndexMap<String, Option<VolumeConfig>>,
	/// Named networks; a `None` value is a default-configured network (`name:` with no body).
	#[serde(default)]
	pub networks: IndexMap<String, Option<NetworkConfig>>,
	/// Named secrets available to services.
	#[serde(default)]
	pub secrets: IndexMap<String, SecretConfig>,
	/// Named configs available to services.
	#[serde(default)]
	pub configs: IndexMap<String, ConfigConfig>,
	/// Top-level `models:` element (Compose v2.38). Parsed for fidelity; podup
	/// runs no model runner, so the diagnostics pass reports declared models as
	/// not honored.
	#[serde(default)]
	pub models: IndexMap<String, ModelConfig>,
	/// Top-level `x-*` extension fields, preserved and round-tripped via `config` subcommand.
	#[serde(flatten, default, skip_serializing_if = "IndexMap::is_empty")]
	pub extensions: IndexMap<String, serde_yaml::Value>,
}

/// Compose-spec top-level extension key that opts the project into running
/// every service inside one Podman pod. Mirrors the `x-podman-autoupdate`
/// shape: a top-level `x-` extension key parsed from the file's captured
/// extension map so the compose file stays portable to docker compose. The
/// value is a boolean; `true` enables the feature, anything else is rejected
/// with a message naming the key (see [`ComposeFile::podman_pod`]).
pub const X_PODMAN_POD: &str = "x-podman-pod";

/// Placeholder substituted for inline secret/config `content:` values when the
/// file is rendered for display.
pub const REDACTED_PLACEHOLDER: &str = "<redacted>";

impl ComposeFile {
	/// Replace every inline `content:` value under `secrets:` and `configs:`
	/// with [`REDACTED_PLACEHOLDER`], so rendering the file for display (the
	/// `config` subcommand) never writes secret material to stdout. References
	/// to a `file:` or `environment:` source are left untouched: they name a
	/// source, they do not embed the value.
	pub fn redact_inline_content(&mut self) {
		for secret in self.secrets.values_mut() {
			if secret.content.is_some() {
				secret.content = Some(REDACTED_PLACEHOLDER.to_string());
			}
		}
		for config in self.configs.values_mut() {
			if config.content.is_some() {
				config.content = Some(REDACTED_PLACEHOLDER.to_string());
			}
		}
	}

	/// Drop every captured unknown key that the diagnostics pass reports as
	/// "ignored", so a rendered `config` reflects only what podup actually
	/// applies. Compose-spec `x-*` extension keys are kept (they are valid and
	/// round-tripped) but a typo or an unmapped key is removed rather than
	/// echoed back, which is what makes re-feeding the output stop re-triggering
	/// the same warning. Mirrors the levels walked by the diagnostics collector.
	pub fn strip_ignored_unknown_keys(&mut self) {
		retain_extension_keys(&mut self.extensions);
		for model in self.models.values_mut() {
			retain_extension_keys(&mut model.unknown);
		}
		for net in self.networks.values_mut().flatten() {
			retain_extension_keys(&mut net.unknown);
			if let Some(ipam) = net.ipam.as_mut() {
				retain_extension_keys(&mut ipam.unknown);
			}
		}
		for vol in self.volumes.values_mut().flatten() {
			retain_extension_keys(&mut vol.unknown);
		}
		for svc in self.services.values_mut() {
			retain_extension_keys(&mut svc.unknown);
			if let Some(hc) = svc.healthcheck.as_mut() {
				retain_extension_keys(&mut hc.unknown);
			}
			if let Some(deploy) = svc.deploy.as_mut() {
				retain_extension_keys(&mut deploy.unknown);
			}
			if let Some(develop) = svc.develop.as_mut() {
				for rule in &mut develop.watch {
					retain_extension_keys(&mut rule.unknown);
				}
			}
			if let Some(cred) = svc.credential_spec.as_mut() {
				retain_extension_keys(&mut cred.unknown);
			}
			if let Some(provider) = svc.provider.as_mut() {
				retain_extension_keys(&mut provider.unknown);
			}
		}
	}

	/// Whether the `x-podman-pod: true` extension is set on this compose file.
	/// `Ok(true)` opts the project into one-pod-per-project semantics;
	/// `Ok(false)` (the default) keeps the existing per-network shape. A
	/// value that is not a YAML boolean is rejected with a message naming the
	/// key, so a typo or stray quote is reported rather than silently dropped.
	///
	/// Mirrors `Service::podman_autoupdate`, which reads the same shape from a
	/// per-service unknown map; here the key is at the top level.
	pub fn podman_pod(&self) -> std::result::Result<bool, String> {
		let Some(raw) = self.extensions.get(X_PODMAN_POD) else {
			return Ok(false);
		};
		match raw.as_bool() {
			Some(value) => Ok(value),
			None => Err(format!("{X_PODMAN_POD} must be a boolean (expected: true)")),
		}
	}
}

/// Keep only Compose-spec `x-*` extension keys in a captured-unknowns map,
/// discarding the keys the diagnostics pass warns are ignored.
fn retain_extension_keys(map: &mut IndexMap<String, serde_yaml::Value>) {
	map.retain(|k, _| k.starts_with("x-"));
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
