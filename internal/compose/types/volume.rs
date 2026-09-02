//! Volume, secret, and config mount types.
//!
//! [`VolumeMount`] covers the `volumes:` list on a service (short and long forms).
//! [`VolumeConfig`] describes top-level named volume definitions.
//! [`ServiceSecretRef`] and [`ServiceConfigRef`] are the per-service `secrets:` /
//! `configs:` attachment points (short = just the name, long = full options).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::Labels;

/// Volume mount type: `volume`, `bind`, `tmpfs`, `npipe`, or `cluster`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VolumeType {
	/// A named or anonymous managed volume.
	Volume,
	/// A host path bind mount.
	Bind,
	/// An in-memory tmpfs mount.
	Tmpfs,
	/// A Windows named-pipe mount.
	Npipe,
	/// A cluster (Swarm) volume.
	Cluster,
}

/// Sub-options for a `bind`-type volume mount.
///
/// `#[non_exhaustive]` (3.0.0), like every compose option block below: these
/// structs grow a field whenever the compose surface does, and each addition
/// used to be a breaking change for anyone building the literal. Construct via
/// `Default::default()` and assign the fields you need.
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct BindOptions {
	/// Mount propagation mode (e.g. `rprivate`, `rshared`).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub propagation: Option<String>,
	/// Whether to create the host path if it does not exist.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub create_host_path: Option<bool>,
	/// SELinux relabeling option (`z` shared or `Z` private).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub selinux: Option<String>,
}

/// Sub-options for a `volume`-type mount — nocopy flag and optional driver config.
///
/// `#[non_exhaustive]` (3.0.0): see [`BindOptions`].
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct VolumeOptions {
	/// Whether to skip copying existing target contents into the volume.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub nocopy: Option<bool>,
	/// Labels applied to the volume.
	#[serde(default)]
	pub labels: Labels,
	/// Volume driver name and options.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver_config: Option<DriverConfig>,
	/// Path within the volume to mount instead of its root.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub subpath: Option<String>,
	/// Mount with `noexec`, denying execution of binaries from the volume.
	///
	/// A podup extension, like its two siblings below: the Compose
	/// Specification defines no per-mount hardening flags on the long form,
	/// while the short form has always carried them as raw mount options
	/// (`cache:/app/cache:noexec`). The long form deserved the same reach —
	/// these are the standard mount-hardening trio, and a compose file that
	/// spells a mount out longhand should not lose them (#1160).
	#[serde(skip_serializing_if = "Option::is_none")]
	pub noexec: Option<bool>,
	/// Mount with `nosuid`, ignoring setuid/setgid bits on files in the volume.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub nosuid: Option<bool>,
	/// Mount with `nodev`, denying access to device nodes in the volume.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub nodev: Option<bool>,
}

/// Driver name and key-value options nested under `VolumeOptions`.
///
/// `#[non_exhaustive]` (3.0.0): see [`BindOptions`].
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DriverConfig {
	/// Volume driver name.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Driver-specific options.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub options: HashMap<String, String>,
}

/// Sub-options for a `tmpfs`-type mount — size and mode.
///
/// `#[non_exhaustive]` (3.0.0): see [`BindOptions`].
#[non_exhaustive]
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TmpfsOptions {
	/// Size of the tmpfs mount in bytes.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub size: Option<u64>,
	/// File mode of the tmpfs mount, stored as the actual permission bits.
	///
	/// A compose `mode:` is conventionally octal, so every spelling is normalised
	/// to the same permission bits: a leading-zero string (`0700`), an explicit
	/// `0o700`, and a bare `700` all yield `0o700` (448). Invalid input is a clear
	/// error rather than a silent re-interpretation.
	#[serde(
		default,
		deserialize_with = "deserialize_octal_mode",
		skip_serializing_if = "Option::is_none"
	)]
	pub mode: Option<u32>,
}

/// Deserialize a tmpfs `mode` as octal-aware permission bits.
///
/// A compose `mode:` is conventionally octal, but serde_yaml decodes the spelling
/// before we see it: a `0o`-prefixed literal arrives as its numeric value
/// (`0o700` → 448), a leading-zero literal (`0700`) arrives as a string, and a
/// bare `700` arrives unchanged. We normalise all of them to the actual
/// permission bits (see [`int_mode_bits`] for the integer case), so the renderer
/// can octal-encode a single canonical value. A string is parsed as octal,
/// accepting an optional `0o` prefix and rejecting non-octal digits with a clear
/// message.
fn deserialize_octal_mode<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
	D: serde::Deserializer<'de>,
{
	use serde::de::Error;

	#[derive(Deserialize)]
	#[serde(untagged)]
	enum Raw {
		Int(u32),
		Str(String),
	}

	match Option::<Raw>::deserialize(deserializer)? {
		None => Ok(None),
		Some(Raw::Int(n)) => Ok(Some(int_mode_bits(n))),
		Some(Raw::Str(s)) => {
			let trimmed = s.trim();
			let digits = trimmed
				.strip_prefix("0o")
				.or_else(|| trimmed.strip_prefix("0O"))
				.unwrap_or(trimmed);
			u32::from_str_radix(digits, 8).map(Some).map_err(|_| {
				D::Error::custom(format!(
					"invalid mode {s:?}: use octal notation like 0700 or 0o700"
				))
			})
		}
	}
}

/// Interpret a bare YAML integer `mode:` as octal permission bits.
///
/// A bare `700` is the octal file-mode the user typed, so its decimal digits are
/// read as octal (`700` → `0o700` = 448). A digit of `8` or `9` cannot be a real
/// octal digit, so such a value can only be a `0o` literal that serde_yaml has
/// already decoded to its numeric value (`0o700` → 448, `0o755` → 493); it is
/// taken as the actual permission bits verbatim. The same fallback covers an
/// out-of-range octal reading, so the conversion never panics.
fn int_mode_bits(n: u32) -> u32 {
	u32::from_str_radix(&n.to_string(), 8).unwrap_or(n)
}

/// A volume mount entry — either a short-form string or a long-form typed block.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum VolumeMount {
	/// Short form: a `source:target[:options]` string.
	Short(String),
	/// Long form: an explicitly typed mount with per-type options.
	Long {
		/// Mount type selecting which options block applies.
		#[serde(rename = "type")]
		volume_type: VolumeType,
		/// Mount source — host path, volume name, or omitted for anonymous/tmpfs.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		source: Option<String>,
		/// Mount target path inside the container.
		target: String,
		/// Whether the mount is read-only.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		read_only: Option<bool>,
		/// Bind-specific options, when `volume_type` is `bind`.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		bind: Option<BindOptions>,
		/// Volume-specific options, when `volume_type` is `volume`.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		volume: Option<VolumeOptions>,
		/// Tmpfs-specific options, when `volume_type` is `tmpfs`.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		tmpfs: Option<TmpfsOptions>,
		/// Mount consistency requirement (a no-op outside Docker Desktop).
		#[serde(default, skip_serializing_if = "Option::is_none")]
		consistency: Option<String>,
	},
}

impl VolumeMount {
	/// Returns the container-side target path of the mount.
	pub fn target(&self) -> &str {
		match self {
			VolumeMount::Short(s) => {
				let parts: Vec<&str> = s.splitn(3, ':').collect();
				if parts.len() >= 2 {
					parts[1]
				} else {
					parts[0]
				}
			}
			VolumeMount::Long { target, .. } => target,
		}
	}
}

/// Named volume definition in the top-level `volumes:` block.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[non_exhaustive]
pub struct VolumeConfig {
	/// Volume driver name; the runtime default is used if absent.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,
	/// Driver-specific options.
	#[serde(default, skip_serializing_if = "HashMap::is_empty")]
	pub driver_opts: HashMap<String, String>,
	/// Whether the volume is externally managed and not created by podup.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub external: Option<bool>,
	/// Custom volume name overriding the project-prefixed default.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,
	/// Labels applied to the volume.
	#[serde(default)]
	pub labels: Labels,
	/// Unrecognized keys preserved verbatim for round-tripping.
	#[serde(flatten, default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
	pub unknown: indexmap::IndexMap<String, serde_yaml::Value>,
}

/// Reference to a named config from a service — short form (name only) or long form with mount target.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ServiceConfigRef {
	/// Short form: the name of a top-level config to mount.
	Short(String),
	/// Long form: a config name with mount target and ownership options.
	Long {
		/// Name of the top-level config to mount.
		source: String,
		/// Mount path inside the container; defaults to `/<source>` if absent.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		target: Option<String>,
		/// Owner UID of the mounted file.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		uid: Option<String>,
		/// Owner GID of the mounted file.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		gid: Option<String>,
		/// File permission mode of the mounted file. Octal notation per the
		/// Compose Specification (`0444`, `0o444`, or a bare number), so a
		/// leading-zero literal like `0444` is read as octal, not decimal.
		#[serde(
			default,
			deserialize_with = "deserialize_octal_mode",
			skip_serializing_if = "Option::is_none"
		)]
		mode: Option<u32>,
	},
}

impl ServiceConfigRef {
	/// Returns the name of the referenced top-level config.
	pub fn source(&self) -> &str {
		match self {
			ServiceConfigRef::Short(s) => s,
			ServiceConfigRef::Long { source, .. } => source,
		}
	}

	/// Returns the container mount target, if specified.
	pub fn target(&self) -> Option<&str> {
		match self {
			ServiceConfigRef::Short(_) => None,
			ServiceConfigRef::Long { target, .. } => target.as_deref(),
		}
	}
}

/// Reference to a named secret from a service — short form (name only) or long form with mount target.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ServiceSecretRef {
	/// Short form: the name of a top-level secret to mount.
	Short(String),
	/// Long form: a secret name with mount target and ownership options.
	Long {
		/// Name of the top-level secret to mount.
		source: String,
		/// Mount path; defaults to `/run/secrets/<source>` if absent.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		target: Option<String>,
		/// Owner UID of the mounted file.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		uid: Option<String>,
		/// Owner GID of the mounted file.
		#[serde(default, skip_serializing_if = "Option::is_none")]
		gid: Option<String>,
		/// File permission mode of the mounted file. Octal notation per the
		/// Compose Specification (`0444`, `0o444`, or a bare number), so a
		/// leading-zero literal like `0444` is read as octal, not decimal.
		#[serde(
			default,
			deserialize_with = "deserialize_octal_mode",
			skip_serializing_if = "Option::is_none"
		)]
		mode: Option<u32>,
	},
}

impl ServiceSecretRef {
	/// Returns the name of the referenced top-level secret.
	pub fn source(&self) -> &str {
		match self {
			ServiceSecretRef::Short(s) => s,
			ServiceSecretRef::Long { source, .. } => source,
		}
	}

	/// Returns the container mount target, if specified.
	pub fn target(&self) -> Option<&str> {
		match self {
			ServiceSecretRef::Short(_) => None,
			ServiceSecretRef::Long { target, .. } => target.as_deref(),
		}
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
