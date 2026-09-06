//! Compose file parsing, `extends:` resolution, `include:` merging, and
//! topological service ordering.

pub mod types;

mod anchor;
mod diagnostics;
mod extends;
mod include;
mod merge;
mod order;
mod tags;
mod validate;

use std::path::{Path, PathBuf};

use crate::error::{ComposeError, Result};
use crate::substitute;
use types::{ComposeFile, ServiceNetworks};

pub use order::{resolve_levels, resolve_order};
pub use validate::validate_config;

/// Whether a compose-file path is the stdin sentinel `-` (`docker compose -f -`).
fn is_stdin(path: &Path) -> bool {
	path == Path::new("-")
}

/// Parse a compose file from disk, applying variable substitution and
/// resolving `extends:` / `include:` directives.
pub fn parse_file(path: &Path) -> Result<ComposeFile> {
	parse_file_with_env_files(path, &[])
}

/// Like [`parse_file`], additionally loading `env_files` (the global
/// `--env-file` flag) into the variable map used for interpolation. These take
/// effect for the top-level file and any included files.
///
/// They **replace** a project `.env` rather than adding to it: when `env_files`
/// is non-empty, `.env` is not read. That is docker-correct, and the opposite
/// of what this comment used to claim, which also reached docs.rs readers. The
/// process environment still takes precedence over both.
pub fn parse_file_with_env_files(path: &Path, env_files: &[String]) -> Result<ComposeFile> {
	parse_file_with_env_files_interp(path, env_files, true)
}

/// Like [`parse_file_with_env_files`] but with explicit control over variable
/// interpolation. `interpolate = false` (the `config --no-interpolate` path)
/// leaves `${VAR}` placeholders literal while still resolving
/// `extends:`/`include:`/merge.
pub fn parse_file_with_env_files_interp(
	path: &Path,
	env_files: &[String],
	interpolate: bool,
) -> Result<ComposeFile> {
	// `-f -` reads the compose document from stdin (like `docker compose`); there
	// is no file to canonicalize, so relative paths and `.env` resolve against the
	// working directory.
	let (abs, dir) = if is_stdin(path) {
		let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
		(PathBuf::from("-"), cwd)
	} else {
		let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
		let dir = abs.parent().unwrap_or(Path::new(".")).to_path_buf();
		(abs, dir)
	};
	let mut file = parse_file_inner_with_env(&abs, &dir, env_files, interpolate)?;

	let includes = std::mem::take(&mut file.include);
	for inc in includes {
		let (extra_env_files, project_dir_override) = match &inc {
			types::IncludeConfig::Long {
				env_file,
				project_directory,
				..
			} => (
				env_file.as_ref().map(|ef| ef.to_list()).unwrap_or_default(),
				project_directory.as_ref().map(|pd| dir.join(pd)),
			),
			_ => (vec![], None),
		};
		for rel in inc.paths() {
			let rel_path = std::path::Path::new(&rel);
			// The Compose Specification resolves `include` paths relative to the
			// including file and treats `../` as canonical (monorepos routinely use
			// `include: ../shared/compose.yaml`). An absolute path is used as given.
			// This matches docker-compose and the trusted-input policy already
			// applied to `extends.file` and `env_file`: the compose file is
			// trusted input, like a Makefile.
			let inc_path = if rel_path.is_absolute() {
				rel_path.to_path_buf()
			} else {
				dir.join(&rel)
			};
			let inc_dir = project_dir_override.clone().unwrap_or_else(|| {
				inc_path
					.parent()
					.map(|p| p.to_path_buf())
					.unwrap_or_else(|| dir.clone())
			});
			let mut combined_env_files = env_files.to_vec();
			combined_env_files.extend(extra_env_files.iter().cloned());
			let mut included = include::parse_included_file(
				&inc_path,
				&inc_dir,
				&combined_env_files,
				interpolate,
			)?;
			anchor::anchor_compose_file(&mut included, &inc_dir);
			include::merge_compose_file(&mut file, included);
		}
	}

	extends::resolve_all_extends(&mut file, &dir)?;
	Ok(file)
}

/// Collect parse-time diagnostics for an already-parsed compose file: warnings
/// about recognized-but-unsupported keys and fields that are accepted but carry
/// no effect on Podman. The CLI prints these automatically; library consumers
/// (e.g. panel-agent) can call this to surface the same warnings, since
/// [`parse_file`] does not emit them itself.
pub fn collect_diagnostics(file: &ComposeFile) -> Vec<String> {
	diagnostics::collect(file)
}

/// Parse and merge multiple compose files (the `-f`/`COMPOSE_FILE` list).
///
/// Files are merged left to right: a later file overrides an earlier one,
/// service by service (per-field, like `extends`), with top-level
/// volumes/networks/secrets/configs replaced on key conflicts. Relative paths
/// resolve against the first file's directory, matching the compose project
/// directory. `env_files` feed interpolation for every file.
pub fn parse_files_with_env_files(paths: &[PathBuf], env_files: &[String]) -> Result<ComposeFile> {
	parse_files_with_env_files_interp(paths, env_files, true)
}

/// Like [`parse_files_with_env_files`] but with explicit interpolation control.
/// `interpolate = false` backs `config --no-interpolate`: `${VAR}` placeholders
/// are left literal across all merged files.
pub fn parse_files_with_env_files_interp(
	paths: &[PathBuf],
	env_files: &[String],
	interpolate: bool,
) -> Result<ComposeFile> {
	let mut iter = paths.iter();
	let first = iter
		.next()
		.ok_or_else(|| ComposeError::FileNotFound("no compose file given".to_string()))?;
	let mut merged = parse_file_with_env_files_interp(first, env_files, interpolate)?;
	for path in iter {
		let other = parse_file_with_env_files_interp(path, env_files, interpolate)?;
		// `!override`/`!reset` are attached to keys in the raw document and are
		// gone by the time it is a typed `ComposeFile`, so they are collected
		// from the file itself and passed alongside.
		let directives = tags::collect_from_file(path);
		merge_override(&mut merged, other, &directives);
	}
	normalize_default_network(&mut merged);
	// Semantic validation runs only on the interpolated file: `--no-interpolate`
	// leaves literal `${VAR}` placeholders that cannot be reference- or
	// range-checked. This makes `config`, `up`, and `generate` reject the same
	// contradictory files docker-compose does, at config time.
	if interpolate {
		validate::validate(&merged)?;
	}
	for warning in diagnostics::collect(&merged) {
		tracing::warn!("{warning}");
	}
	// Unknown keys nested inside option blocks (bind/volume/tmpfs mounts, long-form
	// service networks, deploy.resources specs) are dropped by the typed model and
	// so are invisible to `diagnostics::collect`. Re-read each input file's raw,
	// interpolated YAML and diff those blocks directly. This runs per input file
	// (pre-merge): an unknown key in ANY `-f` file should warn, and `-` (stdin) is
	// skipped because the parse above already consumed it and it cannot be re-read.
	for path in paths {
		if is_stdin(path) {
			continue;
		}
		let Ok(yaml) = interpolated_yaml_text(path, env_files, interpolate) else {
			continue;
		};
		for warning in diagnostics::raw_nested_unknown_warnings(&yaml) {
			tracing::warn!("{warning}");
		}
	}
	Ok(merged)
}

/// Re-read `path` and return the interpolated, merge-resolved YAML as text, the
/// same document shape the parser builds before deserializing into a
/// `ComposeFile`. Used only by the raw nested-key diagnostic, which needs the
/// pre-typed document to spot keys the model drops. `interpolate = false` (the
/// `config --no-interpolate` path) leaves `${VAR}` placeholders literal.
fn interpolated_yaml_text(path: &Path, env_files: &[String], interpolate: bool) -> Result<String> {
	let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
	let dir = abs.parent().unwrap_or(Path::new(".")).to_path_buf();
	let content = crate::filesystem::read_to_string_capped(&abs).map_err(|e| {
		if e.kind() == std::io::ErrorKind::NotFound {
			ComposeError::FileNotFound(abs.display().to_string())
		} else {
			ComposeError::Io(e)
		}
	})?;
	let value = if interpolate {
		let vars = if env_files.is_empty() {
			substitute::build_vars(&dir)
		} else {
			substitute::build_vars_with_env_files_strict(&dir, env_files)?
		};
		merge::interpolated_value(&content, Some(&vars))?
	} else {
		merge::interpolated_value(&content, None)?
	};
	Ok(serde_yaml::to_string(&value)?)
}

/// Synthesize the implicit `default` network, matching docker-compose: any
/// service that declares neither `networks:` nor `network_mode` is attached to
/// a project `default` network. Without this, such services are created with no
/// network namespace at all: they get no IP and cannot resolve each other by
/// name, silently breaking the common no-`networks:`-block compose file.
///
/// The `default` network is created as `{project}_default` (see
/// `resolve_network_name`) unless the file already defines a top-level
/// `networks.default`, whose configuration is then respected. Idempotent.
pub(crate) fn normalize_default_network(file: &mut ComposeFile) {
	let needs_default = file
		.services
		.values()
		.any(|svc| svc.network_mode.is_none() && matches!(svc.networks, ServiceNetworks::Empty));
	if !needs_default {
		return;
	}
	file.networks.entry("default".to_string()).or_insert(None);
	for svc in file.services.values_mut() {
		if svc.network_mode.is_none() && matches!(svc.networks, ServiceNetworks::Empty) {
			svc.networks = ServiceNetworks::List(vec!["default".to_string()]);
		}
	}
}

/// Merge `other` into `target` with `other` winning (compose `-f` override
/// semantics): services are merged field-by-field, other top-level maps replace
/// on key conflict.
fn merge_override(target: &mut ComposeFile, other: ComposeFile, directives: &tags::Directives) {
	for (name, svc) in other.services {
		if let Some(base) = target.services.get_mut(&name) {
			let tagged = directives.get(&name);
			*base = extends::merge_service_tagged(std::mem::take(base), svc, tagged);
		} else {
			target.services.insert(name, svc);
		}
	}
	for (k, v) in other.volumes {
		target.volumes.insert(k, v);
	}
	for (k, v) in other.networks {
		target.networks.insert(k, v);
	}
	for (k, v) in other.secrets {
		target.secrets.insert(k, v);
	}
	for (k, v) in other.configs {
		target.configs.insert(k, v);
	}
	for (k, v) in other.models {
		target.models.insert(k, v);
	}
}

/// Parse a compose YAML string (no file I/O).
///
/// Variable substitution is applied using only the process environment.
/// `extends: { file: ... }` and `include:` directives are not resolved;
/// use [`parse_file`] for that.
pub fn parse_str(content: &str) -> Result<ComposeFile> {
	let vars = substitute::build_vars(Path::new("."));
	let mut file = merge::deserialize_with_merge_interp(content, Some(&vars))?;
	extends::resolve_extends_same_file(&mut file)?;
	Ok(file)
}

/// Parse raw (already-substituted) YAML into a `ComposeFile` without any
/// post-processing.
pub fn parse_str_raw(content: &str) -> Result<ComposeFile> {
	merge::deserialize_with_merge(content)
}

pub(crate) fn parse_file_inner(path: &Path, dir: &Path) -> Result<ComposeFile> {
	parse_file_inner_with_env(path, dir, &[], true)
}

pub(crate) fn parse_file_inner_with_env(
	path: &Path,
	dir: &Path,
	extra_env_files: &[String],
	interpolate: bool,
) -> Result<ComposeFile> {
	let content = if is_stdin(path) {
		crate::filesystem::read_stdin_to_string_capped().map_err(ComposeError::Io)?
	} else {
		crate::filesystem::read_to_string_capped(path).map_err(|e| {
			if e.kind() == std::io::ErrorKind::NotFound {
				ComposeError::FileNotFound(path.display().to_string())
			} else {
				ComposeError::Io(e)
			}
		})?
	};
	// `config --no-interpolate` leaves `${VAR}` placeholders literal; otherwise
	// interpolate against the env/.env/env-file variable map. Interpolation runs
	// on the parsed YAML scalars (see `deserialize_with_merge_interp`), not the
	// raw text, so resolved values cannot alter the document structure.
	if interpolate {
		let vars = if extra_env_files.is_empty() {
			substitute::build_vars(dir)
		} else {
			substitute::build_vars_with_env_files_strict(dir, extra_env_files)?
		};
		merge::deserialize_with_merge_interp(&content, Some(&vars))
	} else {
		merge::deserialize_with_merge(&content)
	}
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
