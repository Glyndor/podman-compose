use super::{
	override_for, resolve_base_dir, resolve_compose_files, resolve_project_name,
	sanitize_project_name, validate_project_directory,
};
use std::path::{Path, PathBuf};

#[test]
fn validate_project_directory_accepts_none_and_existing_dir() {
	validate_project_directory(None).unwrap();
	let dir = std::env::temp_dir();
	validate_project_directory(Some(&dir)).unwrap();
}

#[test]
fn validate_project_directory_rejects_missing_and_file() {
	let missing = std::env::temp_dir().join(format!("podup-pd-{}-nope", std::process::id()));
	assert!(validate_project_directory(Some(&missing)).is_err());

	let file = std::env::temp_dir().join(format!("podup-pd-{}.tmp", std::process::id()));
	std::fs::write(&file, b"x").unwrap();
	assert!(validate_project_directory(Some(&file)).is_err());
	let _ = std::fs::remove_file(&file);
}

#[test]
fn explicit_compose_files_win() {
	let p = resolve_compose_files(&[PathBuf::from("custom.yml")]);
	assert_eq!(p, vec![PathBuf::from("custom.yml")]);
}

#[test]
fn multiple_explicit_compose_files_preserved() {
	let p = resolve_compose_files(&[PathBuf::from("a.yml"), PathBuf::from("b.yml")]);
	assert_eq!(p, vec![PathBuf::from("a.yml"), PathBuf::from("b.yml")]);
}

#[test]
fn missing_compose_file_falls_back_to_default_name() {
	// In a directory with no candidate files, the default name is returned
	// so the resulting error names a sensible path.
	let dir = std::env::temp_dir().join(format!("podup-cf-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let prev = std::env::current_dir().unwrap();
	std::env::set_current_dir(&dir).unwrap();
	// Scope COMPOSE_FILE to "unset" race-free so a value set in the test
	// environment cannot leak in and `temp-env` restores it afterwards.
	let p = temp_env::with_var_unset("COMPOSE_FILE", || resolve_compose_files(&[]));
	std::env::set_current_dir(prev).unwrap();
	let _ = std::fs::remove_dir_all(&dir);
	assert_eq!(p, vec![PathBuf::from("docker-compose.yml")]);
}

#[test]
fn project_directory_override_wins() {
	let base = resolve_base_dir(
		Some(Path::new("/srv/app")),
		Path::new("/etc/compose/docker-compose.yml"),
	);
	assert_eq!(base, PathBuf::from("/srv/app"));
}

#[test]
fn defaults_to_compose_file_parent() {
	let base = resolve_base_dir(None, Path::new("/etc/compose/docker-compose.yml"));
	assert_eq!(base, PathBuf::from("/etc/compose"));
}

#[test]
fn bare_filename_resolves_to_current_dir() {
	// A bare filename has no directory component, so the base directory must
	// fall back to the working directory — never an empty path, which would
	// leave a relative `file:` source to be resolved against the Podman
	// service's working directory ($HOME) instead of the project directory.
	let base = resolve_base_dir(None, Path::new("docker-compose.yml"));
	assert_eq!(base, std::env::current_dir().unwrap());
	assert!(base.is_absolute());
}

#[test]
fn explicit_project_name_wins() {
	let name = resolve_project_name(
		Some("explicit".to_string()),
		Some("from-compose"),
		Path::new("/srv/myapp"),
	);
	assert_eq!(name, "explicit");
}

#[test]
fn compose_name_used_when_no_explicit() {
	let name = resolve_project_name(None, Some("from-compose"), Path::new("/srv/myapp"));
	assert_eq!(name, "from-compose");
}

#[cfg(unix)]
#[test]
fn falls_back_to_directory_basename() {
	let dir = std::env::temp_dir().join(format!("podup-pn-{}", std::process::id()));
	std::fs::create_dir_all(&dir).unwrap();
	let name = resolve_project_name(None, None, &dir);
	let _ = std::fs::remove_dir_all(&dir);
	// The basename is sanitized; the temp dir name is already lowercase
	// alphanumeric with hyphens, so it survives unchanged.
	assert_eq!(
		name,
		dir.file_name().unwrap().to_string_lossy().to_lowercase()
	);
}

#[test]
fn sanitize_lowercases_and_drops_invalid_chars() {
	assert_eq!(sanitize_project_name("My App!"), "myapp");
}

#[test]
fn sanitize_keeps_underscore_and_hyphen() {
	assert_eq!(sanitize_project_name("web_service-1"), "web_service-1");
}

#[test]
fn sanitize_strips_leading_separators() {
	assert_eq!(sanitize_project_name("__leading"), "leading");
	assert_eq!(sanitize_project_name("--dash"), "dash");
}

#[test]
fn sanitize_empty_result_falls_back_to_podup() {
	assert_eq!(sanitize_project_name("!!!"), "podup");
	assert_eq!(sanitize_project_name(""), "podup");
}

/// #1077: base plus `docker-compose.override.yml` is how nearly every
/// repository separates dev from prod, and docker compose merges it
/// automatically when no `-f` is given. podup ran the base alone, silently.
#[test]
fn auto_discovery_picks_up_the_override_file() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("compose.yaml"), b"services: {}\n").unwrap();
	std::fs::write(dir.path().join("compose.override.yaml"), b"services: {}\n").unwrap();
	let found = override_for(&dir.path().join("compose.yaml"));
	assert_eq!(found, Some(dir.path().join("compose.override.yaml")));
}

/// Only the first override in precedence order is used — docker compose does
/// not merge two of them.
#[test]
fn only_the_highest_precedence_override_is_used() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("compose.yaml"), b"services: {}\n").unwrap();
	for name in [
		"compose.override.yml",
		"docker-compose.override.yaml",
		"compose.override.yaml",
	] {
		std::fs::write(dir.path().join(name), b"services: {}\n").unwrap();
	}
	assert_eq!(
		override_for(&dir.path().join("compose.yaml")),
		Some(dir.path().join("compose.override.yaml")),
		"compose.override.yaml outranks the rest"
	);
}

/// No override beside the base is the ordinary case and must stay silent.
#[test]
fn no_override_file_is_not_an_error() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("compose.yaml"), b"services: {}\n").unwrap();
	assert_eq!(override_for(&dir.path().join("compose.yaml")), None);
}

/// An explicit `-f` means the caller is choosing the file set, so the
/// override is not added behind their back. docker compose skips it too.
#[test]
fn explicit_files_suppress_override_discovery() {
	let explicit = vec![std::path::PathBuf::from("only.yaml")];
	assert_eq!(resolve_compose_files(&explicit), explicit);
}
