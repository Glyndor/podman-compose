//! Anchor relative paths of externally-imported content to their source dir.
//!
//! A service pulled in via `include:` or an external `extends: { file: ... }`
//! carries paths (build context, env_file, bind-mount sources, secret/config
//! files) that the compose spec resolves relative to the directory of the file
//! that defined them, not the top-level project directory. The engine resolves
//! every relative path against a single project base directory, so before such
//! content is merged we rewrite its relative paths to absolute paths anchored at
//! the imported file's directory. Absolute paths then pass through the engine's
//! base-directory resolution unchanged.

use std::path::Path;

use super::types::{
	BuildConfig, ComposeFile, EnvFile, EnvFileEntry, Service, VolumeMount, VolumeType,
};

/// Anchor every path-bearing field of an imported compose file to `dir`.
pub(super) fn anchor_compose_file(file: &mut ComposeFile, dir: &Path) {
	for svc in file.services.values_mut() {
		anchor_service(svc, dir);
	}
	for secret in file.secrets.values_mut() {
		if let Some(f) = secret.file.as_deref().and_then(|f| anchor_fs(f, dir)) {
			secret.file = Some(f);
		}
	}
	for config in file.configs.values_mut() {
		if let Some(f) = config.file.as_deref().and_then(|f| anchor_fs(f, dir)) {
			config.file = Some(f);
		}
	}
}

/// Anchor the path-bearing fields of a single service to `dir`.
pub(super) fn anchor_service(svc: &mut Service, dir: &Path) {
	if let Some(build) = svc.build.as_mut() {
		match build {
			BuildConfig::Context(c) => {
				if let Some(a) = anchor_context(c, dir) {
					*c = a;
				}
			}
			BuildConfig::Config { context, .. } => {
				// An absent `context` defaults to the project directory `.`;
				// anchor that default to the included file's directory so a
				// `dockerfile_inline`-only build resolves against the right dir.
				let effective = context.as_deref().unwrap_or(".");
				if let Some(a) = anchor_context(effective, dir) {
					*context = Some(a);
				}
			}
		}
	}

	match &mut svc.env_file {
		EnvFile::Empty => {}
		EnvFile::Single(e) => anchor_env_entry(e, dir),
		EnvFile::List(list) => {
			for e in list.iter_mut() {
				anchor_env_entry(e, dir);
			}
		}
	}

	for v in svc.volumes.iter_mut() {
		match v {
			VolumeMount::Short(s) => {
				if let Some(a) = anchor_short_volume(s, dir) {
					*s = a;
				}
			}
			VolumeMount::Long {
				volume_type: VolumeType::Bind,
				source: Some(src),
				..
			} => {
				if let Some(a) = anchor_bind(src, dir) {
					*src = a;
				}
			}
			VolumeMount::Long { .. } => {}
		}
	}
}

/// A path is anchorable when it is relative and not a `~` home reference
/// (home expansion happens later, against the user's home, not the file dir).
fn anchor_fs(path: &str, dir: &Path) -> Option<String> {
	if path.is_empty() || path.starts_with('~') || Path::new(path).is_absolute() {
		return None;
	}
	Some(dir.join(path).to_string_lossy().into_owned())
}

/// Build contexts may be Git/URL references, which must not be anchored.
fn anchor_context(context: &str, dir: &Path) -> Option<String> {
	if context.contains("://") || context.starts_with("git@") {
		return None;
	}
	anchor_fs(context, dir)
}

/// Only explicit relative bind sources (`./x`, `../x`) are anchored; named
/// volumes, absolute paths, and `~` references are left untouched.
fn anchor_bind(src: &str, dir: &Path) -> Option<String> {
	src.starts_with('.')
		.then(|| dir.join(src).to_string_lossy().into_owned())
}

fn anchor_short_volume(s: &str, dir: &Path) -> Option<String> {
	let parts: Vec<&str> = s.splitn(3, ':').collect();
	let src = parts.first()?;
	if !src.starts_with('.') {
		return None;
	}
	let mut rebuilt = dir.join(src).to_string_lossy().into_owned();
	for part in &parts[1..] {
		rebuilt.push(':');
		rebuilt.push_str(part);
	}
	Some(rebuilt)
}

fn anchor_env_entry(entry: &mut EnvFileEntry, dir: &Path) {
	match entry {
		EnvFileEntry::Path(p) => {
			if let Some(a) = anchor_fs(p, dir) {
				*p = a;
			}
		}
		EnvFileEntry::Config { path, .. } => {
			if let Some(a) = anchor_fs(path, dir) {
				*path = a;
			}
		}
	}
}

#[cfg(test)]
#[path = "anchor_tests.rs"]
mod tests;
