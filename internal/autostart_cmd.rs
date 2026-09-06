//! The `autostart` command group: manage a rootless `systemctl --user` unit that
//! brings the compose stack up at boot. Split out of `main::run` so that function
//! stays within the source line limit; `install` and `status` work from the
//! compose file alone and never contact Podman, while `uninstall --purge` is the
//! only branch that connects (to run the `down -v` teardown).

use std::path::PathBuf;

use podup::compose::types::ComposeFile;
use podup::ComposeError;

use crate::cli::{AutostartCommands, AutostartMode};

/// The slice of CLI globals the `autostart` dispatch needs, gathered up so the
/// already-consumed `Cli` (its `project` is moved earlier) need not be borrowed
/// whole.
pub(crate) struct AutostartEnv<'a> {
	pub profile: &'a [String],
	pub env_files: &'a [String],
	pub socket: Option<String>,
	/// Connection-pool cap forwarded to [`Client::with_pool_size`]. `None`
	/// means use [`Client::DEFAULT_POOL_SIZE`].
	pub connection_pool_size: Option<usize>,
}

/// Handle the `autostart` command group. `install` and `status` never contact
/// Podman; `uninstall --purge` is the only branch that connects, to run the
/// `down -v` teardown.
pub(crate) async fn dispatch(
	env: &AutostartEnv<'_>,
	compose_files: &[PathBuf],
	project: String,
	base_dir: PathBuf,
	file: &ComposeFile,
	kind: &AutostartCommands,
) -> podup::Result<()> {
	match kind {
		AutostartCommands::Install {
			mode: AutostartMode::Quadlet,
			no_start,
			dry_run,
			auto_update,
		} => {
			if let Some(interval) = auto_update {
				return Err(ComposeError::Autostart(format!(
					"--auto-update is only valid with --mode service (got {}); quadlet mode already \
					 drives auto-update through podman-auto-update.timer",
					interval.as_str()
				)));
			}
			// Quadlet mode hands the stack to systemd as native units rendered from
			// the compose file. It still needs the base directory absolute: a
			// `.build` unit's context is resolved by the systemd generator with no
			// cwd, so a relative context would look under the unit directory.
			let base_dir = std::fs::canonicalize(&base_dir).unwrap_or(base_dir);
			podup::autostart::install_quadlet(
				&podup::autostart::RealSystemCtl,
				file,
				&project,
				&base_dir,
				*no_start,
				*dry_run,
			)
		}
		AutostartCommands::Install {
			mode: AutostartMode::Service,
			no_start,
			dry_run,
			auto_update,
		} => {
			// systemd has no relative-path context, so resolve the exe, every compose
			// file, and the working directory to absolute paths the unit can embed.
			let exe = std::env::current_exe().map_err(|e| {
				ComposeError::Autostart(format!("cannot locate the podup executable: {e}"))
			})?;
			let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
			let mut abs_files = Vec::with_capacity(compose_files.len());
			for f in compose_files {
				abs_files.push(std::fs::canonicalize(f).map_err(|e| {
					ComposeError::Autostart(format!(
						"cannot resolve compose file {}: {e}",
						f.display()
					))
				})?);
			}
			let working_dir = std::fs::canonicalize(&base_dir).unwrap_or(base_dir);
			// The longest stop_grace_period in the project. systemd bounds the whole
			// ExecStop independently of what podup does inside it, and its default
			// is 90s, so without this a stack whose slowest container needs longer
			// is killed mid-stop at reboot, while a manual `podup stop` honours it.
			let max_grace = podup::autostart::max_stop_grace_secs(file);
			let unit = podup::autostart::ServiceUnitOpts::new(exe, abs_files, project, working_dir)
				.with_profiles(env.profile.to_vec())
				.with_env_files(env.env_files.to_vec())
				.with_max_stop_grace_secs(max_grace);
			let opts = podup::autostart::InstallOptions::new(unit)
				.with_no_start(*no_start)
				.with_dry_run(*dry_run)
				.with_auto_update_interval(auto_update.map(|i| i.as_str().to_string()));
			podup::autostart::install(&podup::autostart::RealSystemCtl, &opts)
		}
		AutostartCommands::Install {
			mode: AutostartMode::Start,
			no_start,
			dry_run,
			auto_update,
		} => {
			if let Some(interval) = auto_update {
				return Err(ComposeError::Autostart(format!(
					"--auto-update is only valid with --mode service (got {}); start mode has no \
					 compose front-end on the boot path to run",
					interval.as_str()
				)));
			}
			// Start mode's whole point is that the boot path holds no compose
			// file, so the container name has to be resolved here, once, and
			// baked into the unit. `sole_container` is also the refusal: it is
			// what rejects a multi-service or scaled project and names the mode
			// to use instead.
			let container = podup::autostart::sole_container(file, &project)
				.map_err(|why| ComposeError::Autostart(why.to_string()))?;
			// systemd resolves an exec line with no PATH of its own, so the unit
			// needs podman by absolute path rather than by name.
			let podman = resolve_podman()?;
			let opts = podup::autostart::StartUnitOpts::new(podman, project.clone(), container)
				.with_stop_grace_secs(podup::autostart::max_stop_grace_secs(file));

			// The one check that needs a live Podman. Start mode restores what
			// the store holds and deliberately does not reconcile, so a
			// container that is absent, or present but no longer matching the
			// file, must be refused HERE, at install, where a human is
			// watching. At boot there is no podup to notice: that is the cost of
			// keeping it off the boot path, not an oversight.
			//
			// Skipped under --dry-run, which exists to show the unit without
			// touching anything, including the socket.
			if !*dry_run {
				let client = podup::podman::connect_with_pool_size(
					env.socket.as_deref(),
					env.connection_pool_size
						.unwrap_or(podup::Client::DEFAULT_POOL_SIZE),
				)?;
				let engine =
					podup::Engine::with_base_dir(client, project.clone(), base_dir.clone());
				precheck_start_mode(&engine, file, &opts.container).await?;
			}

			podup::autostart::install_start(
				&podup::autostart::RealSystemCtl,
				&opts,
				*dry_run,
				*no_start,
			)
		}
		AutostartCommands::Uninstall { purge } => {
			// Remove whichever mode is installed; the two never coexist, and asking
			// the user to name the mode only risks a no-op against the wrong one.
			// Hold the uninstall's outcome rather than `?`-ing it here. By the time
			// it can fail, the unit files are already gone and `installed_mode`
			// would no longer recognise the project, so short-circuiting would
			// skip `--purge` exactly when the stack is still up and most needs
			// tearing down, leaving its named volumes behind and the state
			// unrecognisable. Purge first, report the failure after.
			let uninstalled = match podup::autostart::installed_mode(&project) {
				podup::autostart::InstalledMode::Quadlet => {
					podup::autostart::uninstall_quadlet(&podup::autostart::RealSystemCtl, &project)
				}
				// Service or nothing installed: the service uninstall is idempotent and
				// prints "already removed" when there is nothing there.
				_ => podup::autostart::uninstall(&podup::autostart::RealSystemCtl, &project),
			};
			if *purge {
				// `--purge` is the only autostart branch that touches Podman: tear the
				// stack down and remove its named volumes via the normal `down -v` path.
				let client = podup::podman::connect_with_pool_size(
					env.socket.as_deref(),
					env.connection_pool_size
						.unwrap_or(podup::Client::DEFAULT_POOL_SIZE),
				)?;
				let engine = podup::Engine::with_base_dir(client, project, base_dir);
				let _lock = engine.lock_project()?;
				engine.down_with_options(file, true).await?;
			}
			uninstalled
		}
		AutostartCommands::Status => {
			podup::autostart::status(&podup::autostart::RealSystemCtl, &project)
		}
		AutostartCommands::Rebuild { service } => podup::autostart::rebuild_quadlet(
			&podup::autostart::RealSystemCtl,
			&project,
			service.as_deref(),
		),
	}
}

/// The absolute path to `podman`, looked up on `PATH`. A systemd exec line has
/// no `PATH` of its own, so a bare name in the unit would fail at boot with
/// nothing to point at.
fn resolve_podman() -> podup::Result<PathBuf> {
	let path = std::env::var_os("PATH").ok_or_else(|| {
		ComposeError::Autostart("PATH is not set, so podman cannot be located".to_string())
	})?;
	for dir in std::env::split_paths(&path) {
		let candidate = dir.join("podman");
		if candidate.is_file() {
			return Ok(std::fs::canonicalize(&candidate).unwrap_or(candidate));
		}
	}
	Err(ComposeError::Autostart(
		"cannot find `podman` on PATH; start mode's unit runs it directly, so it needs an \
		 absolute path at install time"
			.to_string(),
	))
}

/// Refuse to install start mode over a container that is absent or has drifted
/// from the compose file.
///
/// Both refusals are the mode stating its own contract. It restores what Podman
/// holds rather than reconciling, so a missing container means a deploy that
/// never completed, and a hash mismatch means the file has moved on since the
/// container was created. Booting would silently resume the old configuration
/// in the second case, which is the mirror of the `up -d` hazard and worse,
/// because `up -d` at least converges.
async fn precheck_start_mode(
	engine: &podup::Engine,
	file: &ComposeFile,
	container: &str,
) -> podup::Result<()> {
	let (name, service) = file
		.services
		.iter()
		.next()
		.expect("sole_container already established exactly one service");
	let expected = engine.expected_config_hash(service, file)?;
	match engine.container_config_hash(container).await? {
		None => Err(ComposeError::Autostart(format!(
			"start mode needs the container to exist already: no container named \
			 '{container}' for service '{name}'.\n\
			 Start mode resumes what Podman holds and never creates anything, so run \
			 `podup up -d` first, then install."
		))),
		Some(found) if found != expected => Err(ComposeError::Autostart(format!(
			"container '{container}' no longer matches the compose file: it was created from \
			 config {found}, the file now renders {expected}.\n\
			 Start mode restores rather than reconciles, so installing now would boot the old \
			 configuration silently. Run `podup up -d` to bring the container up to date, then \
			 install."
		))),
		Some(_) => Ok(()),
	}
}
