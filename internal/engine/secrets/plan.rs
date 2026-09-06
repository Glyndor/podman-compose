//! Pure mapping from compose `secrets:`/`configs:` references to native-secret
//! plans. No daemon access, so the mapping is unit-testable; the create and
//! preflight side effects live in [`super`]'s `Engine` impl.

use std::path::{Path, PathBuf};

use crate::compose::types::{ComposeFile, Service, ServiceConfigRef, ServiceSecretRef};
use crate::error::{ComposeError, Result};

use super::super::staging;
use super::secret_bytes::SecretBytes;

/// Podman's hard limit on secret payload size (from `containers/common`): the
/// payload must be larger than 0 and strictly smaller than this many bytes.
pub(super) const MAX_SECRET_BYTES: usize = 512_000;

/// Where the bytes of a podup-created secret come from.
///
/// A `file:` source carries the resolved host path rather than its contents:
/// this module maps compose to plans with no I/O at all, which is what keeps the
/// mapping unit-testable, so the read happens in the effectful layer that creates
/// the secret.
pub(super) enum Payload {
	/// Inline `content:`/`environment:`: the bytes, already resolved.
	Inline(SecretBytes),
	/// `file:` is the resolved host path to read at creation time.
	File(PathBuf),
}

/// A planned native secret for a service: the Podman secret `source` to attach,
/// the in-container `target`, optional permissions, and, for every source podup
/// creates itself, the `payload` to create under `source`. `external: true`
/// references carry no payload (the secret must pre-exist).
pub(super) struct NativePlan {
	pub(super) source: String,
	pub(super) target: String,
	pub(super) mode: Option<u32>,
	pub(super) uid: Option<u32>,
	pub(super) gid: Option<u32>,
	pub(super) payload: Option<Payload>,
}

/// The fields of a `secrets:`/`configs:` definition that decide where its bytes
/// come from. A borrowed view, so the two distinct compose types (`SecretConfig`
/// and `ConfigConfig`) resolve through one function.
struct SourceDef<'a> {
	content: Option<&'a str>,
	environment: Option<&'a str>,
	file_source: Option<&'a str>,
	external: bool,
	external_name: Option<&'a str>,
}

/// Where a secret/config's bytes come from once the compose def is resolved.
enum Source {
	/// Inline `content:`/`environment:`: `(scoped podman name, payload bytes)`.
	Inline(String, SecretBytes),
	/// `file:`: `(scoped podman name, resolved host path)`.
	File(String, PathBuf),
	/// `external: true`: name of the pre-existing podman secret.
	External(String),
}

/// Collect the native-secret plans for a service without touching the daemon. A
/// dangerous `mode:` (execute/setuid/setgid/sticky) is rejected here so a
/// hostile mode never reaches Podman.
pub(super) fn collect_native_plans(
	project: &str,
	service: &Service,
	file: &ComposeFile,
	base_dir: &Path,
) -> Result<Vec<NativePlan>> {
	let mut plans = Vec::new();

	for secret_ref in &service.secrets {
		let (name, target_override, mode, uid, gid) = secret_ref_parts(secret_ref);
		if let Some(def) = file.secrets.get(&name) {
			let source = resolve_source(
				project,
				"secret",
				&name,
				SourceDef {
					content: def.content.as_deref(),
					environment: def.environment.as_deref(),
					file_source: def.file.as_deref(),
					external: def.external == Some(true),
					external_name: def.name.as_deref(),
				},
				base_dir,
			)?;
			// A bare target name lands under /run/secrets/<name>, which is where
			// Podman mounts a secret referenced by name and where a `file:` source
			// landed back when it was a bind mount. Changing it would move every
			// existing project's secrets.
			push_plan(
				&mut plans,
				source,
				target_override.unwrap_or(name),
				mode,
				uid,
				gid,
			)?;
		}
	}

	for config_ref in &service.configs {
		let (name, target_override, mode, uid, gid) = config_ref_parts(config_ref);
		if let Some(def) = file.configs.get(&name) {
			let source = resolve_source(
				project,
				"config",
				&name,
				SourceDef {
					content: def.content.as_deref(),
					environment: def.environment.as_deref(),
					file_source: def.file.as_deref(),
					external: def.external == Some(true),
					external_name: def.name.as_deref(),
				},
				base_dir,
			)?;
			// Configs default to an absolute container-root path: `/name`, not
			// `/run/secrets/name`. That is what separates a config from a secret
			// here, and it is the path a `file:` config landed on when it was a
			// bind mount.
			let target = target_override.unwrap_or_else(|| format!("/{name}"));
			push_plan(&mut plans, source, target, mode, uid, gid)?;
		}
	}

	Ok(plans)
}

/// Resolve a secret/config definition to its native [`Source`]. `external`
/// wins (it may also carry a custom `name:`); every other populated source,
/// inline `content:`/`environment:` and `file:` alike, becomes a project-scoped
/// native secret. An empty def yields `None` and contributes no plan.
fn resolve_source(
	project: &str,
	kind: &str,
	name: &str,
	def: SourceDef<'_>,
	base_dir: &Path,
) -> Result<Option<Source>> {
	let SourceDef {
		content,
		environment,
		file_source,
		external,
		external_name,
	} = def;
	if external {
		return Ok(Some(Source::External(
			external_name.unwrap_or(name).to_string(),
		)));
	}
	let podup_created = content.is_some() || environment.is_some() || file_source.is_some();
	if podup_created && !staging::is_safe_project_name(name) {
		// The name becomes part of the project-scoped Podman secret name and a URL
		// query parameter, so require a bounded, well-formed identifier rather than
		// an arbitrary (possibly huge or control-laden) YAML key.
		return Err(ComposeError::Unsupported(format!(
			"{kind} name {name:?} must be ASCII alphanumeric/dash/underscore/dot, \
			 at most 128 chars, and not start with a dot"
		)));
	}
	if let Some(content) = content {
		return Ok(Some(Source::Inline(
			scoped_name(project, kind, name),
			SecretBytes::new(content.as_bytes().to_vec()),
		)));
	}
	if let Some(env_var) = environment {
		let value = std::env::var(env_var).map_err(|_| {
			ComposeError::Unsupported(format!(
				"{kind} '{name}' references env var '{env_var}' which is not set"
			))
		})?;
		return Ok(Some(Source::Inline(
			scoped_name(project, kind, name),
			SecretBytes::new(value.into_bytes()),
		)));
	}
	if let Some(host_path) = file_source {
		// Resolve like a bind-mount source: a relative `file:` is anchored to the
		// project dir (not the Podman service's cwd) and `~` is expanded, the same
		// handling `volumes:` gets, and the same this had when it was a bind.
		return Ok(Some(Source::File(
			scoped_name(project, kind, name),
			PathBuf::from(super::super::container::resolve_bind_source(
				host_path, base_dir,
			)),
		)));
	}
	Ok(None)
}

/// Append a [`NativePlan`] for a resolved source, dropping an empty def and
/// rejecting a dangerous `mode:` before the spec is built. `uid`/`gid` are
/// numeric in libpod, so a non-numeric value (a user/group name) is dropped to
/// the default rather than erroring.
fn push_plan(
	plans: &mut Vec<NativePlan>,
	source: Option<Source>,
	target: String,
	mode: Option<u32>,
	uid: Option<String>,
	gid: Option<String>,
) -> Result<()> {
	let (source, payload, from_file) = match source {
		None => return Ok(()),
		Some(Source::Inline(s, p)) => (s, Some(Payload::Inline(p)), false),
		Some(Source::File(s, p)) => (s, Some(Payload::File(p)), true),
		Some(Source::External(s)) => (s, None, false),
	};
	// Default to the Compose Specification's world-readable `0444` when no `mode:`
	// is given. A Podman-native secret otherwise mounts at `0000`, which a non-root
	// container user cannot read (only root reads it via DAC override), diverging
	// from docker-compose where the default is readable.
	//
	// A `file:` source is the exception: it is left unset here so the effectful
	// layer can mirror the host file's own permission bits. That keeps what the
	// container sees identical to the bind this used to be: a `0600` secret stays
	// unreadable to a non-root container user instead of being widened to `0444`.
	let mode = if from_file {
		mode
	} else {
		mode.or(Some(0o444))
	};
	if let Some(m) = mode {
		staging::reject_dangerous_secret_mode(m, &source)?;
	}
	plans.push(NativePlan {
		source,
		target,
		mode,
		uid: uid.and_then(|s| s.parse().ok()),
		gid: gid.and_then(|s| s.parse().ok()),
		payload,
	});
	Ok(())
}

/// Project-scoped Podman secret name for an inline secret/config, namespaced by
/// `kind` so a secret and a config sharing a compose name do not collide.
pub(super) fn scoped_name(project: &str, kind: &str, name: &str) -> String {
	format!("{project}_{kind}_{name}")
}

/// Reject a payload Podman would refuse (`len == 0` or `>= MAX_SECRET_BYTES`),
/// with a clearer message than the daemon's opaque 500.
pub(super) fn check_secret_size(name: &str, len: usize) -> Result<()> {
	if len == 0 || len >= MAX_SECRET_BYTES {
		return Err(ComposeError::Unsupported(format!(
			"secret '{name}' is {len} bytes; a Podman secret payload must be \
			 larger than 0 and smaller than {MAX_SECRET_BYTES} bytes"
		)));
	}
	Ok(())
}

/// Whether a secret/config def is one podup creates as a project-scoped native
/// secret, inline `content:`/`environment:` or a `file:` source. `external:`
/// wins and is never created (nor removed) by podup.
pub(super) fn is_podup_created_source(
	external: Option<bool>,
	content: Option<&str>,
	environment: Option<&str>,
	file_source: Option<&str>,
) -> bool {
	external != Some(true) && (content.is_some() || environment.is_some() || file_source.is_some())
}

/// The permission bits to mount a `file:` secret with when the compose file
/// names no `mode:`: the host file's own, so the container sees what it saw
/// when this was a bind mount.
///
/// Execute and the special bits are masked off: a secret holds data, never code,
/// and letting a `0755` host file through would trip the dangerous-mode guard and
/// fail an `up` that used to work. Unreadable metadata falls back to the Compose
/// Specification's `0444`; the create call fails right after with a clearer
/// message than a permissions guess would give.
pub(super) fn host_file_secret_mode(path: &Path) -> u32 {
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		match std::fs::metadata(path) {
			Ok(md) => md.permissions().mode() & 0o666,
			Err(_) => 0o444,
		}
	}
	#[cfg(not(unix))]
	{
		let _ = path;
		0o444
	}
}

/// Decompose a secret reference into `(name, target, mode, uid, gid)`.
fn secret_ref_parts(
	r: &ServiceSecretRef,
) -> (
	String,
	Option<String>,
	Option<u32>,
	Option<String>,
	Option<String>,
) {
	match r {
		ServiceSecretRef::Short(s) => (s.clone(), None, None, None, None),
		ServiceSecretRef::Long {
			source,
			target,
			mode,
			uid,
			gid,
		} => (
			source.clone(),
			target.clone(),
			*mode,
			uid.clone(),
			gid.clone(),
		),
	}
}

/// Decompose a config reference into `(name, target, mode, uid, gid)`.
fn config_ref_parts(
	r: &ServiceConfigRef,
) -> (
	String,
	Option<String>,
	Option<u32>,
	Option<String>,
	Option<String>,
) {
	match r {
		ServiceConfigRef::Short(s) => (s.clone(), None, None, None, None),
		ServiceConfigRef::Long {
			source,
			target,
			mode,
			uid,
			gid,
		} => (
			source.clone(),
			target.clone(),
			*mode,
			uid.clone(),
			gid.clone(),
		),
	}
}

#[cfg(test)]
#[path = "plan_tests.rs"]
mod tests;
