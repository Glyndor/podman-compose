//! Translate a parsed compose file into Podman Quadlet unit files.
//!
//! Quadlet is Podman's systemd integration: declarative `.container`,
//! `.network` and `.volume` units placed under
//! `~/.config/containers/systemd/` that a systemd generator turns into
//! services, so systemd owns the lifecycle (boot, restart, dependencies)
//! instead of a long-running `podup` process.
//!
//! This is an additive export path, not a replacement for the runner. It
//! maps the common compose fields and warns — loudly, never silently — for
//! every field that is set but has no Quadlet equivalent yet, so generated
//! units never quietly drop configuration.

mod render;
mod unit;
mod warnings;

use std::cell::Cell;

use crate::compose::types::ComposeFile;
use unit::{build_unit, container_unit, network_unit, volume_unit, UnitContext};

thread_local! {
	/// CLI `--no-warn` flag honoured by the Quadlet path's host-binding /
	/// privilege-escalation warnings. Set by `write_quadlet` when the CLI
	/// parsed `--no-warn` and consulted by `container_unit` before each
	/// `tracing::warn!` so the operator can silence the per-generate noise
	/// the same way they do on `up`/`create`/`run`/`exec`.
	///
	/// The live engine path carries the flag on `Engine::no_warn`; the Quadlet
	/// path goes through a free function and has no such struct, so a
	/// thread-local is the smallest non-breaking plumbing. The Quadlet command
	/// runs synchronously on the CLI thread, so a thread-local has the same
	/// observable behaviour as a parameter would.
	static NO_WARN: Cell<bool> = const { Cell::new(false) };
}

/// Set the Quadlet-path `--no-warn` flag for the current thread; restores the
/// previous value on drop. The CLI driver wraps its call to `write_quadlet`
/// in this scope so `container_unit` can read it via `is_no_warn_set`.
pub struct NoWarnGuard {
	prev: bool,
}

impl Default for NoWarnGuard {
	fn default() -> Self {
		Self::new()
	}
}

impl NoWarnGuard {
	/// Activate `--no-warn` for the current thread until the guard is dropped.
	/// Invoked by the CLI driver around `write_quadlet`; embedders that drive
	/// the Quadlet path directly can use the same guard for the same effect.
	pub fn new() -> Self {
		let prev = NO_WARN.with(|c| c.get());
		NO_WARN.with(|c| c.set(true));
		Self { prev }
	}
}

impl Drop for NoWarnGuard {
	fn drop(&mut self) {
		NO_WARN.with(|c| c.set(self.prev));
	}
}

/// Whether `--no-warn` is active on the current thread. Read by
/// `container_unit` before each host-binding `tracing::warn!`.
pub(super) fn is_no_warn_set() -> bool {
	NO_WARN.with(|c| c.get())
}

/// The `# podup-owner: <project>` marker a unit carries as its literal first
/// line, if present.
fn marker_owner(contents: &str) -> Option<&str> {
	contents
		.lines()
		.find_map(|line| line.strip_prefix("# podup-owner: "))
}

/// Refuse to overwrite a unit that belongs to another project.
///
/// Unit stems are `{project}-{service}`, so project `app` with service
/// `extra-web` and project `app-extra` with service `web` both produce
/// `app-extra-web.container`. Uninstall already resolves that ambiguity by
/// reading the ownership marker, but writing had no counterpart: installing one
/// project would overwrite the other's unit *and re-stamp the marker*, so the
/// next `uninstall` of the wrong project would delete it.
///
/// An existing file with no marker is left to be overwritten with a warning
/// rather than refused: it is either a unit from a podup old enough to predate
/// the marker, or a foreign file, and hard-failing would strand upgrades.
fn guard_existing_owner(
	path: &std::path::Path,
	filename: &str,
	contents: &str,
) -> std::io::Result<()> {
	let Ok(existing) = std::fs::read_to_string(path) else {
		return Ok(());
	};
	match (marker_owner(&existing), marker_owner(contents)) {
		(Some(existing_owner), Some(new_owner)) if existing_owner != new_owner => {
			Err(std::io::Error::new(
				std::io::ErrorKind::AlreadyExists,
				format!(
					"refusing to overwrite {filename}: it belongs to project '{existing_owner}', not '{new_owner}'"
				),
			))
		}
		(None, _) => {
			tracing::warn!("overwriting {filename}, which carries no podup ownership marker");
			Ok(())
		}
		_ => Ok(()),
	}
}

/// Write a unit private to its owner.
///
/// Units render each `environment:` entry as an `Environment=KEY=VALUE` line,
/// so a compose file's database password ends up in this file verbatim. The
/// default umask would leave it world-readable; systemd reads user units as the
/// owning user, so 0600 costs nothing. Permissions are reset explicitly as well
/// as at creation, so re-installing over a unit written by an older podup
/// tightens it rather than leaving it as it was.
fn write_unit_file(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
	std::fs::write(path, contents)?;
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
	}
	Ok(())
}

/// A single generated unit file: its name and full contents.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct QuadletUnit {
	/// File name, e.g. `web.container` or `db-data.volume`.
	pub filename: String,
	/// Full file contents, ending in a newline.
	pub contents: String,
}

/// The result of a generation run: the units plus any warnings about set but
/// unmapped fields.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct QuadletOutput {
	/// Generated unit files, in a deterministic order.
	pub units: Vec<QuadletUnit>,
	/// Human-readable warnings for compose fields with no Quadlet mapping.
	pub warnings: Vec<String>,
}

impl QuadletOutput {
	/// The first unit file name that two different units share, if any. Distinct
	/// compose keys can sanitize to the same stem (e.g. `web:1` and `web_1` both
	/// become `web_1`); writing them would silently overwrite one unit, dropping
	/// a service/network/volume from the export. Callers surface this as an error
	/// instead of clobbering.
	pub fn duplicate_filename(&self) -> Option<&str> {
		let mut seen = std::collections::HashSet::new();
		self.units
			.iter()
			.find(|u| !seen.insert(u.filename.as_str()))
			.map(|u| u.filename.as_str())
	}
}

/// Write `units` into `dir`, creating it if needed, and return the paths written
/// in order. Defense in depth: refuse any unit whose file name is not a plain path
/// component. The library already sanitizes stems, but a write target must never
/// contain a separator, `.` or `..` that could escape `dir`. Shared by
/// `generate quadlet -o` and `autostart --mode quadlet`, so both place units
/// through the identical safety check.
pub fn write_units(
	dir: &std::path::Path,
	units: &[QuadletUnit],
) -> std::io::Result<Vec<std::path::PathBuf>> {
	std::fs::create_dir_all(dir)?;
	let mut written = Vec::with_capacity(units.len());
	for unit in units {
		if std::path::Path::new(&unit.filename).file_name()
			!= Some(std::ffi::OsStr::new(&unit.filename))
		{
			return Err(std::io::Error::new(
				std::io::ErrorKind::InvalidInput,
				format!("refusing unsafe quadlet unit file name: {}", unit.filename),
			));
		}
		let path = dir.join(&unit.filename);
		guard_existing_owner(&path, &unit.filename, &unit.contents)?;
		write_unit_file(&path, &unit.contents)?;
		written.push(path);
	}
	Ok(written)
}

/// Translate a compose file into Quadlet units for the given project name,
/// resolving relative build contexts against the current directory (the common
/// case: running from the project directory). Use [`generate_at`] to resolve them
/// against an explicit base directory instead.
pub fn generate(file: &ComposeFile, project: &str) -> QuadletOutput {
	generate_at(file, project, &std::env::current_dir().unwrap_or_default())
}

/// As [`generate`], but resolves a service's relative `build:` context against
/// `base_dir` (the compose file's directory) rather than the current directory.
/// The systemd generator runs a `.build` unit with no cwd, so a unit written for
/// it must carry an absolute `SetWorkingDirectory`; pass the compose base here.
///
/// Emits one `.container` per service, one `.network` per declared network,
/// and one `.volume` per declared named volume. Replica scaling, inline-Dockerfile
/// builds, and other fields without a Quadlet mapping are reported as warnings
/// rather than silently dropped.
pub fn generate_at(file: &ComposeFile, project: &str, base_dir: &std::path::Path) -> QuadletOutput {
	let mut out = QuadletOutput::default();

	// External networks/volumes are assumed to pre-exist. Emitting a unit would
	// make systemd try to (re-)create them, so skip them here; the container unit
	// references such resources by their existing name instead.
	for (name, cfg) in &file.networks {
		if cfg.as_ref().is_some_and(|c| c.external == Some(true)) {
			continue;
		}
		out.units.push(network_unit(name, project, cfg.as_ref()));
		// `podman network create` (and therefore Quadlet) exposes no key for IPAM
		// options beyond the IPAM driver, so `ipam.options` cannot be emitted. The
		// live engine forwards them via the libpod API directly, so warn rather than
		// let `generate` silently diverge from `up`.
		if let Some(c) = cfg {
			if let Some(ipam) = &c.ipam {
				if !ipam.options.is_empty() {
					out.warnings.push(format!(
						"network '{name}': ipam.options have no Quadlet key and are not emitted; \
						 the live engine forwards them but `generate` cannot"
					));
				}
			}
		}
	}
	for (name, cfg) in &file.volumes {
		if cfg.as_ref().is_some_and(|c| c.external == Some(true)) {
			continue;
		}
		out.units.push(volume_unit(name, project, cfg.as_ref()));
	}

	let declared_volumes: Vec<&str> = file
		.volumes
		.iter()
		.filter(|(_, cfg)| cfg.as_ref().is_none_or(|c| c.external != Some(true)))
		.map(|(name, _)| name.as_str())
		.collect();
	let declared_networks: Vec<&str> = file
		.networks
		.iter()
		.filter(|(_, cfg)| cfg.as_ref().is_none_or(|c| c.external != Some(true)))
		.map(|(name, _)| name.as_str())
		.collect();
	let ctx = UnitContext {
		project,
		declared_volumes: &declared_volumes,
		declared_networks: &declared_networks,
		secrets: &file.secrets,
		base_dir,
	};
	for (name, service) in &file.services {
		// Emit a `.build` unit first so the systemd generator builds the image
		// before the container that references it via `Image=<stem>.build`.
		if let Some(unit) = build_unit(name, project, service, base_dir, &mut out.warnings) {
			out.units.push(unit);
		}
		out.units
			.push(container_unit(name, service, &ctx, &mut out.warnings));
	}

	out
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod no_warn_tests {
	//! The Quadlet-path host-binding / privilege-escalation warnings are
	//! gated on a thread-local set by [`NoWarnGuard`]. The guard restores the
	//! previous value on drop so nested scopes compose; a test that runs
	//! inside another test's guard (or after one has leaked) would see a
	//! different starting value, so the guard is documented as zero-overhead
	//! for callers that do not wrap `write_quadlet`.
	use super::{is_no_warn_set, NoWarnGuard};

	#[test]
	fn no_warn_is_off_by_default() {
		assert!(!is_no_warn_set());
	}

	#[test]
	fn guard_sets_and_restores() {
		assert!(!is_no_warn_set());
		{
			let _g = NoWarnGuard::new();
			assert!(is_no_warn_set());
		}
		assert!(
			!is_no_warn_set(),
			"dropping the guard must restore the previous value"
		);
	}

	#[test]
	fn nested_guards_restore_in_reverse_order() {
		let _outer = NoWarnGuard::new();
		assert!(is_no_warn_set());
		{
			// A nested guard inherits the inner value (true), so dropping it
			// leaves the outer guard's setting intact.
			let _inner = NoWarnGuard::new();
		}
		assert!(
			is_no_warn_set(),
			"a nested NoWarnGuard must not stomp the outer guard's value"
		);
	}
}

#[cfg(all(test, unix))]
mod write_guard_tests {
	use std::os::unix::fs::PermissionsExt;

	use super::{write_units, QuadletUnit};

	fn unit(filename: &str, owner: &str) -> QuadletUnit {
		QuadletUnit {
			filename: filename.to_string(),
			contents: format!(
				"# podup-owner: {owner}\n[Container]\nEnvironment=PGPASSWORD=hunter2\n"
			),
		}
	}

	#[test]
	fn refuses_to_overwrite_a_sibling_projects_unit() {
		// `app` + service `extra-web` and `app-extra` + service `web` collide on
		// one filename. Overwriting would also re-stamp the marker, so the next
		// uninstall of the wrong project would delete the survivor.
		let dir = tempfile::tempdir().expect("tempdir");
		write_units(dir.path(), &[unit("app-extra-web.container", "app-extra")]).expect("first");

		let err = write_units(dir.path(), &[unit("app-extra-web.container", "app")])
			.expect_err("must refuse");
		assert!(
			format!("{err}").contains("belongs to project 'app-extra'"),
			"got: {err}"
		);

		let kept =
			std::fs::read_to_string(dir.path().join("app-extra-web.container")).expect("read");
		assert!(
			kept.contains("# podup-owner: app-extra"),
			"the original owner's marker must survive"
		);
	}

	#[test]
	fn rewriting_your_own_unit_is_allowed() {
		let dir = tempfile::tempdir().expect("tempdir");
		write_units(dir.path(), &[unit("app-web.container", "app")]).expect("first");
		write_units(dir.path(), &[unit("app-web.container", "app")]).expect("second");
	}

	#[test]
	fn units_are_written_private_because_they_carry_environment_values() {
		let dir = tempfile::tempdir().expect("tempdir");
		let written = write_units(dir.path(), &[unit("app-web.container", "app")]).expect("write");
		let mode = std::fs::metadata(&written[0])
			.expect("stat")
			.permissions()
			.mode() & 0o777;
		assert_eq!(
			mode, 0o600,
			"a unit holding Environment= secrets must not be world-readable"
		);
	}

	#[test]
	fn an_existing_unit_written_by_an_older_podup_is_tightened() {
		let dir = tempfile::tempdir().expect("tempdir");
		let path = dir.path().join("app-web.container");
		std::fs::write(&path, "[Container]\n").expect("seed");
		std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

		write_units(dir.path(), &[unit("app-web.container", "app")]).expect("write");

		let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
		assert_eq!(
			mode, 0o600,
			"re-installing must tighten a unit left loose by an older version"
		);
	}
}
