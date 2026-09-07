//! `podup autostart --mode quadlet`: hand the whole stack to systemd as native
//! Podman Quadlet units under the rootless `~/.config/containers/systemd/`.
//!
//! Where service mode installs one unit that shells out to `podup up` at boot,
//! quadlet mode writes the same `.container`/`.build`/`.volume`/`.network` units
//! `generate quadlet` emits, so systemd owns boot, restart and dependency
//! ordering directly. The generated `.container` units already carry
//! `[Install] WantedBy=default.target`, so a `daemon-reload` wires them into boot
//! on its own, so this module writes them, reloads, and starts them now; it never
//! `enable`s a generated unit (systemd does not enable generator output).

use std::path::{Path, PathBuf};

use crate::compose::types::ComposeFile;
use crate::{quadlet, ComposeError};

use super::{checked, config_home, emit_guards, unit_path, SystemCtl};

/// `${XDG_CONFIG_HOME:-~/.config}/containers/systemd/`, where Quadlet reads a
/// user's units from. The same directory `generate quadlet` documents.
pub fn quadlet_dir() -> PathBuf {
	config_home().join("containers").join("systemd")
}

/// The `.service` names systemd derives from the generated `.container` units:
/// Quadlet turns `<stem>.container` into `<stem>.service`.
fn container_services(units: &[quadlet::QuadletUnit]) -> Vec<String> {
	units
		.iter()
		.filter_map(|u| u.filename.strip_suffix(".container"))
		.map(|stem| format!("{stem}.service"))
		.collect()
}

/// The project name recorded in a generated unit file's `# podup-owner:`
/// marker, or `None` if the file carries no such marker.
///
/// This deliberately does NOT read the `Label=podup.project=<project>` line
/// every unit builder in `crate::quadlet` also stamps (that label stays, but
/// only for its original purpose; Podman uses `podup.project` for
/// container/secret scoping at runtime). A compose service's user-supplied
/// `labels:` renders into the very same `[Section]`, in the very same
/// `Key=Value` shape, so a service declaring `labels: {podup.project:
/// other}` produces a forged `Label=podup.project=other` line that is
/// textually indistinguishable from the real one, and would be the FIRST
/// such line if it precedes the trusted stamp, defeating a scan that takes
/// the first match. The `# podup-owner: <project>` marker every unit builder
/// emits as its literal first line cannot be forged the same way: systemd
/// treats `#`-prefixed lines as comments, and compose labels only ever
/// render as `Label=key=value` entries, never as a comment line. So this is
/// the one place ownership is decided.
fn unit_owner(path: &Path) -> Option<String> {
	let contents = std::fs::read_to_string(path).ok()?;
	contents
		.lines()
		.find_map(|line| line.strip_prefix("# podup-owner: ").map(str::to_string))
}

/// This project's installed quadlet unit files, sorted. Drives uninstall
/// (remove) and rebuild (find `.build` units).
///
/// A file name starting with `<project>-` is only a candidate: project names
/// may themselves contain `-`, so `app-extra-web.container` also starts with
/// `app-`. Matching on that prefix alone (the old behaviour) meant
/// `uninstall -p app` matched (and `uninstall_quadlet` then stopped and
/// deleted) the sibling project `app-extra`'s units. Each candidate is
/// therefore opened and kept only when its `# podup-owner:` marker equals
/// `project` EXACTLY (see `unit_owner` above); the marker is exact by
/// construction and, unlike the `Label=podup.project=` line, cannot be
/// pre-empted by a forged line from the compose file's own `labels:`.
///
/// A candidate with no marker at all (installed before this ownership check
/// existed) cannot be proven to belong to `project`: treating "no marker" as
/// "assume it's ours" would just reopen the same hole for those legacy
/// installs. So it is left in place, not deleted, and reported via
/// `tracing::warn!` so the user can re-install (which re-marks it) or remove
/// it by hand. Quadlet-mode autostart is recent, so few if any pre-existing
/// unmarked units are expected in the field; leaving one stale file behind
/// for a user to clean up once is a far smaller cost than deleting a
/// sibling project's unit.
fn installed_units(project: &str) -> Vec<PathBuf> {
	let dir = quadlet_dir();
	let prefix = format!("{project}-");
	let mut found = Vec::new();
	if let Ok(entries) = std::fs::read_dir(&dir) {
		for entry in entries.flatten() {
			let path = entry.path();
			if !entry.file_name().to_string_lossy().starts_with(&prefix) {
				continue;
			}
			match unit_owner(&path) {
				Some(owner) if owner == project => found.push(path),
				// A sibling project's unit that happens to share this filename
				// prefix (e.g. `app-extra-web.container` when `project` is `app`);
				// the marker proves it is not ours, so it is left untouched.
				Some(_) => {}
				None => {
					tracing::warn!(
						"quadlet unit {} has no podup-owner ownership marker and cannot be \
						 proven to belong to '{project}'; skipping it rather than risking a \
						 sibling project's unit; re-run `podup autostart install --mode quadlet` \
						 to re-mark it, or remove it by hand if it is stale",
						path.display()
					);
				}
			}
		}
	}
	found.sort();
	found
}

/// Install quadlet-mode autostart: render the stack's units, write them under
/// `~/.config/containers/systemd/`, reload the user manager, and (unless
/// `no_start`) start each container service now. Boot start comes from the units'
/// own `[Install] WantedBy=default.target`, so no `enable` is needed.
pub fn install_quadlet<S: SystemCtl>(
	sc: &S,
	file: &ComposeFile,
	project: &str,
	base_dir: &Path,
	no_start: bool,
	dry_run: bool,
) -> crate::Result<()> {
	// Refuse to stack on top of a service-mode unit for the same project: both
	// would bring the same stack up at boot.
	let service = unit_path(project);
	if service.exists() {
		return Err(ComposeError::Autostart(format!(
			"service-mode autostart unit for '{project}' already exists at {}; \
			 remove it with `podup autostart uninstall` before installing quadlet mode \
			 (both would start the stack at boot).",
			service.display()
		)));
	}

	quadlet::validate_for_quadlet(file)?;
	let result = quadlet::generate_at(file, project, base_dir);
	if let Some(dup) = result.duplicate_filename() {
		return Err(ComposeError::Autostart(format!(
			"quadlet: two resources map to the same unit file {dup:?}; \
			 rename one so their names do not collide after sanitization."
		)));
	}
	for warning in &result.warnings {
		tracing::warn!("{warning}");
	}

	let dir = quadlet_dir();
	let services = container_services(&result.units);
	emit_guards(sc);

	if dry_run {
		for unit in &result.units {
			println!("# {}", unit.filename);
			print!("{}", unit.contents);
		}
		println!(
			"\n# would write {} unit(s) to {}",
			result.units.len(),
			dir.display()
		);
		println!("# would run: systemctl --user daemon-reload");
		if no_start {
			println!("# (--no-start) would not start any container service");
		} else {
			for svc in &services {
				println!("# would run: systemctl --user start {svc}");
			}
		}
		return Ok(());
	}

	let written = quadlet::write_units(&dir, &result.units).map_err(|e| {
		ComposeError::Autostart(format!(
			"cannot write quadlet units to {}: {e}",
			dir.display()
		))
	})?;
	for path in &written {
		eprintln!("podup: wrote {}", path.display());
	}

	checked(sc.systemctl(&["daemon-reload"]), "daemon-reload")?;
	if no_start {
		eprintln!(
			"podup: installed {} quadlet unit(s) for '{project}' (not started; --no-start)",
			written.len()
		);
	} else {
		for svc in &services {
			checked(sc.systemctl(&["start", svc]), &format!("start {svc}"))?;
		}
		eprintln!(
			"podup: started {} container service(s) for '{project}'",
			services.len()
		);
	}
	Ok(())
}

/// Uninstall quadlet-mode autostart: stop this project's container services, remove
/// its `<project>-*` unit files, and reload the user manager. Idempotent: a
/// service that was never started, or a file already gone, is not an error.
///
/// A service that *is* running and refuses to stop is a different matter: the
/// unit files are still removed and the manager still reloaded, but the failure
/// is returned, so uninstall cannot report success while a container keeps
/// running.
pub fn uninstall_quadlet<S: SystemCtl>(sc: &S, project: &str) -> crate::Result<()> {
	let units = installed_units(project);
	let mut first_err: Option<ComposeError> = None;
	// Stop the container services first, while their units still exist.
	for path in &units {
		if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
			if let Some(stem) = name.strip_suffix(".container") {
				let service = format!("{stem}.service");
				// Stop unconditionally. There is no state here worth probing for
				// first: measured against real systemd, `stop` on a unit whose
				// fragment is on disk exits 0 whether it is running, inactive, or
				// was never started at all, and every unit in this loop has its
				// fragment on disk, because that is what `installed_units` found.
				// (`stop` only fails with "not loaded" when no fragment exists,
				// which cannot happen here.)
				//
				// Gating on `is-active` would be worse than redundant: a service
				// that is still coming up reports `activating`, which `is-active`
				// calls a failure, so a stack caught mid-start, or one sitting in
				// `activating (auto-restart)`, would be skipped and left running
				// while uninstall deleted its units and reported success.
				if let Err(e) = checked(sc.systemctl(&["stop", &service]), "stop") {
					tracing::warn!("{e}");
					first_err.get_or_insert(e);
				}
			}
		}
	}
	let mut removed = 0usize;
	for path in &units {
		std::fs::remove_file(path).map_err(|e| {
			ComposeError::Autostart(format!("cannot remove {}: {e}", path.display()))
		})?;
		eprintln!("podup: removed {}", path.display());
		removed += 1;
	}
	if removed == 0 {
		eprintln!("podup: no quadlet autostart units for '{project}' (already removed)");
	}
	checked(sc.systemctl(&["daemon-reload"]), "daemon-reload")?;
	first_err.map_or(Ok(()), Err)
}

/// Rebuild one or all built images of a quadlet-mode install. A `.build` unit is
/// `Type=oneshot`, so its image only rebuilds when the build service is restarted;
/// the container is then restarted to pick up the new image. With `service` given,
/// only that service rebuilds; otherwise every service that has a `.build` unit.
pub fn rebuild_quadlet<S: SystemCtl>(
	sc: &S,
	project: &str,
	service: Option<&str>,
) -> crate::Result<()> {
	let prefix = format!("{project}-");
	let builds: Vec<String> = installed_units(project)
		.iter()
		.filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
		.filter_map(|n| n.strip_suffix(".build").map(String::from))
		.collect();
	if builds.is_empty() {
		return Err(ComposeError::Autostart(format!(
			"no quadlet build units for '{project}'; nothing to rebuild. Only a service \
			 with a compose `build:` produces a `.build` unit, and quadlet-mode autostart \
			 must be installed first (`podup autostart install --mode quadlet`)."
		)));
	}
	let targets: Vec<String> = match service {
		Some(svc) => {
			let stem = format!("{prefix}{svc}");
			if !builds.contains(&stem) {
				let names: Vec<&str> = builds
					.iter()
					.map(|b| b.strip_prefix(&prefix).unwrap_or(b))
					.collect();
				return Err(ComposeError::Autostart(format!(
					"service '{svc}' has no build unit under '{project}'; built services are: {}",
					names.join(", ")
				)));
			}
			vec![stem]
		}
		None => builds,
	};
	for stem in &targets {
		checked(
			sc.systemctl(&["restart", &format!("{stem}-build.service")]),
			&format!("restart {stem}-build.service"),
		)?;
		checked(
			sc.systemctl(&["restart", &format!("{stem}.service")]),
			&format!("restart {stem}.service"),
		)?;
		eprintln!("podup: rebuilt {stem}");
	}
	Ok(())
}

// Unix-gated: the fake `SystemCtl` builds `Output`s via `os::unix` and the paths
// asserted are POSIX. Autostart is a `systemctl --user` feature, so this matches
// the service-mode tests, which gate the same way.
#[cfg(all(test, unix))]
mod tests;
