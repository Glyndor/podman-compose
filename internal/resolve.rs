//! Resolution of compose files, base directory, and project name.

use std::path::{Path, PathBuf};

use podup::ComposeError;

/// Validate an explicit `--project-directory`: it must exist and be a directory.
/// A `None` (unset) directory is always fine: it is derived from the compose
/// file's location. Matches `docker compose`, which errors on a missing working
/// directory instead of silently accepting it.
pub(crate) fn validate_project_directory(dir: Option<&Path>) -> podup::Result<()> {
	if let Some(dir) = dir {
		if !dir.is_dir() {
			return Err(ComposeError::Unsupported(format!(
				"--project-directory {} does not exist or is not a directory",
				dir.display()
			)));
		}
	}
	Ok(())
}

/// Compose-spec file-name precedence, highest first.
const COMPOSE_FILE_CANDIDATES: [&str; 4] = [
	"compose.yaml",
	"compose.yml",
	"docker-compose.yaml",
	"docker-compose.yml",
];

/// Resolve which compose file(s) to load. Explicit `--file` flags win; then the
/// `COMPOSE_FILE` environment variable (a path-separator-delimited list);
/// otherwise probe the compose-spec precedence list in the current directory,
/// falling back to `docker-compose.yml` so a missing-file error names a
/// sensible path. Multiple files are merged in order, later overriding earlier.
/// Whether the compose files came from the operator (`-f` or `COMPOSE_FILE`)
/// rather than from the directory lookup. A named file that is not there is
/// an error even for the commands that can run without any file (#1687).
pub(crate) fn compose_files_were_named(explicit: &[PathBuf]) -> bool {
	!explicit.is_empty() || std::env::var("COMPOSE_FILE").is_ok_and(|v| !v.is_empty())
}

pub(crate) fn resolve_compose_files(explicit: &[PathBuf]) -> Vec<PathBuf> {
	if !explicit.is_empty() {
		return explicit.to_vec();
	}
	if let Ok(env) = std::env::var("COMPOSE_FILE") {
		if !env.is_empty() {
			let sep = if cfg!(windows) { ';' } else { ':' };
			return env.split(sep).map(PathBuf::from).collect();
		}
	}
	for candidate in COMPOSE_FILE_CANDIDATES {
		if Path::new(candidate).is_file() {
			let mut files = vec![PathBuf::from(candidate)];
			files.extend(override_for(Path::new(candidate)));
			return files;
		}
	}
	vec![PathBuf::from("docker-compose.yml")]
}

/// Override-file names, in the compose-spec precedence order. Only the first one
/// present is used; docker compose does not merge two overrides.
const OVERRIDE_FILE_CANDIDATES: [&str; 4] = [
	"compose.override.yaml",
	"compose.override.yml",
	"docker-compose.override.yaml",
	"docker-compose.override.yml",
];

/// The override file to merge on top of an auto-discovered `base`, if one sits
/// beside it.
///
/// Base file plus `docker-compose.override.yml` is how nearly every repository
/// separates dev from prod, and docker compose merges it automatically whenever
/// no explicit `-f` is given. podup ran the base alone and said nothing: wrong
/// image tags, wrong published ports, missing dev bind mounts, exit 0, about a
/// file the user never named on the command line, so nothing in the invocation
/// hinted at what went wrong.
///
/// Discovery is deliberately limited to the auto-discovery path. An explicit
/// `-f` means the caller is choosing the file set themselves, and `COMPOSE_FILE`
/// is that same choice by another name; docker compose skips the override in
/// both cases too.
fn override_for(base: &Path) -> Option<PathBuf> {
	let dir = base.parent().unwrap_or(Path::new(""));
	OVERRIDE_FILE_CANDIDATES
		.iter()
		.map(|name| dir.join(name))
		.find(|path| path.is_file())
}

/// Make a compose-file path absolute for recording on a container label.
///
/// The label is read back by `ls` from a different working directory than the
/// one the project was started in, so a relative path there would be
/// meaningless. Falls back to the path as given when the filesystem cannot
/// resolve it; a best-effort label must never fail the command that sets it.
pub(crate) fn absolute(path: &Path) -> PathBuf {
	std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Resolve the base directory for relative-path resolution. An explicit
/// `--project-directory` wins; otherwise it is the directory containing the
/// compose file (compose-spec default), or the current directory when the
/// compose file has no parent component.
pub(crate) fn resolve_base_dir(project_directory: Option<&Path>, file: &Path) -> PathBuf {
	if let Some(dir) = project_directory {
		return dir.to_path_buf();
	}
	match file.parent() {
		Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
		// A bare compose filename (e.g. `docker-compose.yml`) has an empty parent
		// component. Anchor relative paths to the working directory so a relative
		// `file:` secret/config or bind source resolves against the project
		// directory, not the working directory the Podman service later runs in.
		_ => std::env::current_dir().unwrap_or_default(),
	}
}

/// Resolve the project name following the compose-spec precedence: an explicit
/// `-p` / `COMPOSE_PROJECT_NAME` value, then the top-level `name:` field, then
/// the sanitized basename of the project directory. Explicit values are taken
/// verbatim; only the directory basename is sanitized.
pub(crate) fn resolve_project_name(
	explicit: Option<String>,
	compose_name: Option<&str>,
	base_dir: &Path,
) -> String {
	if let Some(name) = explicit {
		return name;
	}
	if let Some(name) = compose_name {
		return name.to_string();
	}
	// An empty base dir means a bare compose filename in the current directory;
	// canonicalize `.` so the basename comes from the working directory.
	let probe = if base_dir.as_os_str().is_empty() {
		Path::new(".")
	} else {
		base_dir
	};
	let basename = probe
		.canonicalize()
		.unwrap_or_else(|_| probe.to_path_buf())
		.file_name()
		.map(|n| n.to_string_lossy().into_owned())
		.unwrap_or_default();
	sanitize_project_name(&basename)
}

/// Normalize a raw directory name into a valid compose project name: lowercase,
/// keep only `[a-z0-9_-]`, then strip any leading `_`/`-`. Falls back to the
/// `podup` literal when nothing valid remains, so the project name is never
/// empty.
pub(crate) fn sanitize_project_name(raw: &str) -> String {
	let kept: String = raw
		.to_lowercase()
		.chars()
		.filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
		.collect();
	let trimmed = kept.trim_start_matches(['_', '-']);
	if trimmed.is_empty() {
		"podup".to_string()
	} else {
		trimmed.to_string()
	}
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
