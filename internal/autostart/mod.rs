//! `podup autostart` service mode: a single rootless `systemctl --user` unit that
//! brings a compose stack up at boot.
//!
//! Everything here is user-scope only — the unit lives under
//! `${XDG_CONFIG_HOME:-~/.config}/systemd/user/` and every action goes through
//! `systemctl --user` / `loginctl`. No root, no `sudo`, nothing under `/etc` or
//! the system systemd. External-command calls go through the `SystemCtl` seam so
//! the install/uninstall/status logic is unit-testable without a live systemd.

mod quadlet;
mod service;
mod start;

#[cfg(test)]
#[path = "start_tests.rs"]
mod start_tests;

pub use quadlet::{install_quadlet, rebuild_quadlet, uninstall_quadlet};
pub use service::{
	render_service_unit, render_update_service_unit, render_update_timer_unit,
	update_service_file_name, update_timer_file_name, validate_auto_update_interval,
	ServiceUnitOpts,
};
pub use start::{
	render_start_unit, sole_container, validate_start_unit_opts, StartModeRefusal, StartUnitOpts,
};

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::ComposeError;

/// Seam over the external `systemctl --user` / `loginctl` commands. The real impl
/// shells out; tests substitute a fake that records the argument vectors and
/// returns canned output, so install/uninstall/status are exercised without
/// touching the host's systemd.
pub trait SystemCtl {
	/// Run `systemctl --user <args>`.
	fn systemctl(&self, args: &[&str]) -> io::Result<Output>;
	/// Run `loginctl <args>`.
	fn loginctl(&self, args: &[&str]) -> io::Result<Output>;
}

/// The production [`SystemCtl`]: invokes the real `systemctl --user` and
/// `loginctl` binaries.
pub struct RealSystemCtl;

impl SystemCtl for RealSystemCtl {
	fn systemctl(&self, args: &[&str]) -> io::Result<Output> {
		Command::new("systemctl").arg("--user").args(args).output()
	}

	fn loginctl(&self, args: &[&str]) -> io::Result<Output> {
		Command::new("loginctl").args(args).output()
	}
}

/// The longest `stop_grace_period` across a compose file's services, in seconds.
///
/// systemd bounds `ExecStop` independently of what runs inside it, and its
/// default `DefaultTimeoutStopUSec` is 90s. `podup stop` honours each
/// container's own grace period, so a stack whose slowest container needs more
/// than that stops cleanly when a human runs it and gets killed mid-stop at
/// reboot — the difference only shows up during an unattended shutdown, which
/// is the worst version of it.
///
/// `None` when no service sets one, or when none parses, so the unit simply
/// omits `TimeoutStopSec=` and keeps systemd's default. An unparseable duration
/// is skipped rather than defaulted: the value is validated elsewhere, and
/// guessing a timeout from a malformed one would be worse than not setting it.
pub fn max_stop_grace_secs(file: &crate::compose::types::ComposeFile) -> Option<u64> {
	file.services
		.values()
		.filter_map(|s| s.stop_grace_period.as_deref())
		.filter_map(crate::size::parse_duration_secs)
		.max()
}

/// Options for [`install`].
///
/// `#[non_exhaustive]`: see [`ServiceUnitOpts`] — same reason, same construction
/// pattern.
#[non_exhaustive]
#[derive(Default)]
pub struct InstallOptions {
	/// The unit to render and install.
	pub unit: ServiceUnitOpts,
	/// Install the unit but do not `enable --now` it (no immediate start).
	pub no_start: bool,
	/// Print the unit and the actions that would run, but change nothing.
	pub dry_run: bool,
	/// When set, also render and install the sibling `<unit>-update.service`
	/// oneshot and `<unit>-update.timer`. The string is the `OnCalendar=`
	/// word (`hourly`/`daily`/`weekly`).
	pub auto_update_interval: Option<String>,
}

impl InstallOptions {
	/// Install `unit` with the default flags (start it, really write it).
	pub fn new(unit: ServiceUnitOpts) -> Self {
		Self {
			unit,
			no_start: false,
			dry_run: false,
			auto_update_interval: None,
		}
	}

	/// Install the unit but do not `enable --now` it. Builder-style.
	pub fn with_no_start(mut self, no_start: bool) -> Self {
		self.no_start = no_start;
		self
	}

	/// Print what would happen and change nothing. Builder-style.
	pub fn with_dry_run(mut self, dry_run: bool) -> Self {
		self.dry_run = dry_run;
		self
	}

	/// Install the auto-update timer pair alongside the main unit, with the
	/// given `OnCalendar=` word (`hourly`/`daily`/`weekly`). Builder-style.
	pub fn with_auto_update_interval(mut self, interval: Option<String>) -> Self {
		self.auto_update_interval = interval;
		self
	}
}

/// `${XDG_CONFIG_HOME:-~/.config}`. Falls back to `$HOME/.config`, then `.config`
/// in the working directory if even `HOME` is unset (so a path is always formed).
fn config_home() -> PathBuf {
	if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
		return PathBuf::from(x);
	}
	match std::env::var_os("HOME").filter(|s| !s.is_empty()) {
		Some(home) => PathBuf::from(home).join(".config"),
		None => PathBuf::from(".config"),
	}
}

/// Directory that holds `systemctl --user` unit files.
fn unit_dir() -> PathBuf {
	config_home().join("systemd").join("user")
}

/// The unit's file name: `podup-<project>.service`. The project name is validated
/// as a safe path component before reaching here, so it cannot escape `unit_dir`.
fn unit_file_name(project: &str) -> String {
	format!("podup-{project}.service")
}

/// Full path to the unit file for a project.
fn unit_path(project: &str) -> PathBuf {
	unit_dir().join(unit_file_name(project))
}

/// The current login user, for `loginctl` and linger queries. Read from the
/// environment to avoid an `unsafe` `getuid` FFI call.
fn current_user() -> Option<String> {
	std::env::var("USER")
		.ok()
		.or_else(|| std::env::var("LOGNAME").ok())
		.filter(|s| !s.is_empty())
}

/// Quadlet autostart units for this project, if any exist on disk. Service mode
/// and Quadlet mode would both try to start the same stack at boot, so an
/// existing Quadlet install is a conflict to surface, not to silently overwrite.
/// Looks for `<project>-*.container` under
/// `${XDG_CONFIG_HOME:-~/.config}/containers/systemd/`.
fn quadlet_units_present(project: &str) -> Vec<PathBuf> {
	let dir = config_home().join("containers").join("systemd");
	let prefix = format!("{project}-");
	let mut found = Vec::new();
	if let Ok(entries) = std::fs::read_dir(&dir) {
		for entry in entries.flatten() {
			let name = entry.file_name();
			let name = name.to_string_lossy();
			if name.starts_with(&prefix) && name.ends_with(".container") {
				found.push(entry.path());
			}
		}
	}
	found.sort();
	found
}

/// Whether linger is enabled for `user` (so the user manager — and the stack —
/// survives logout and starts at boot). Parses `loginctl show-user <user>
/// --value --property=Linger`, treating any error/unexpected output as "off".
fn linger_enabled<S: SystemCtl>(sc: &S, user: &str) -> bool {
	match sc.loginctl(&["show-user", user, "--value", "--property=Linger"]) {
		Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
			.trim()
			.eq_ignore_ascii_case("yes"),
		_ => false,
	}
}

/// Advisory when linger is off: without it the user manager is not started at
/// boot, so the unit will not bring the stack up until first login. Returns the
/// message to print, or `None` when linger is already enabled.
fn linger_warning<S: SystemCtl>(sc: &S) -> Option<String> {
	let user = current_user()?;
	if linger_enabled(sc, &user) {
		return None;
	}
	Some(format!(
		"linger is not enabled for {user}; the stack will not start at boot until you run:\n    \
		 loginctl enable-linger {user}"
	))
}

/// Advisory when `XDG_RUNTIME_DIR` is unset: `systemctl --user` needs a live user
/// session bus, which is otherwise missing (so the calls below will likely fail).
/// Returns the message to print, or `None` when the variable is set.
fn runtime_dir_warning() -> Option<String> {
	let present = std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|s| !s.is_empty());
	if present {
		return None;
	}
	Some(
		"XDG_RUNTIME_DIR is not set; `systemctl --user` needs an active user session. \
		 Open one (e.g. `machinectl shell <user>@`) or export XDG_RUNTIME_DIR before retrying."
			.to_string(),
	)
}

/// Print the linger and runtime-dir advisories to stderr (warn, never fail).
fn emit_guards<S: SystemCtl>(sc: &S) {
	for warning in [linger_warning(sc), runtime_dir_warning()]
		.into_iter()
		.flatten()
	{
		tracing::warn!("{warning}");
	}
}

/// Turn a `systemctl` invocation result into a `Result`, mapping a launch failure
/// or a non-zero exit into a clear autostart error naming the action.
fn checked(res: io::Result<Output>, what: &str) -> crate::Result<()> {
	let out = res.map_err(|e| {
		ComposeError::Autostart(format!("failed to run `systemctl --user {what}`: {e}"))
	})?;
	if out.status.success() {
		return Ok(());
	}
	let stderr = String::from_utf8_lossy(&out.stderr);
	Err(ComposeError::Autostart(format!(
		"`systemctl --user {what}` failed: {}",
		stderr.trim()
	)))
}

/// Refuse to stack a single-unit mode on top of an existing Quadlet autostart
/// install for the same project: both would start the stack at boot.
fn refuse_if_quadlet_present(project: &str) -> crate::Result<()> {
	let quadlet = quadlet_units_present(project);
	if quadlet.is_empty() {
		return Ok(());
	}
	let names: Vec<String> = quadlet.iter().map(|p| p.display().to_string()).collect();
	Err(ComposeError::Autostart(format!(
		"quadlet autostart units for project '{project}' already exist:\n    {}\n\
		 remove them before installing another mode (quadlet autostart is tracked by #993).",
		names.join("\n    ")
	)))
}

/// Install (and, unless `no_start`, enable + start) the service-mode autostart
/// unit. Writes only under `${XDG_CONFIG_HOME:-~/.config}/systemd/user/`.
///
/// With `opts.auto_update_interval` set, also installs the sibling
/// `<unit>-update.service` (oneshot that runs `podup up -d`) and
/// `<unit>-update.timer` (the schedule that fires it). Removal takes all
/// three down together, see [`uninstall`].
pub fn install<S: SystemCtl>(sc: &S, opts: &InstallOptions) -> crate::Result<()> {
	let project = &opts.unit.project;
	refuse_if_quadlet_present(project)?;

	// Fail closed on values a unit line cannot represent (control characters
	// would inject directives via the literal `WorkingDirectory=` line).
	service::validate_unit_opts(&opts.unit).map_err(ComposeError::Autostart)?;

	if let Some(interval) = &opts.auto_update_interval {
		service::validate_auto_update_interval(interval).map_err(ComposeError::Autostart)?;
	}

	let unit_text = render_service_unit(&opts.unit);
	place_unit(sc, project, &unit_text, opts.dry_run, opts.no_start)?;

	if let Some(interval) = &opts.auto_update_interval {
		place_update_units(
			sc,
			project,
			&opts.unit,
			interval,
			opts.dry_run,
			opts.no_start,
		)?;
	}

	Ok(())
}

/// Install the start-mode unit: `ExecStart=podman start <container>`, so the
/// boot path resumes what Podman already holds instead of reconciling it
/// against the compose file. Single-service projects only; `sole_container`
/// is what refuses the rest, and its refusal names the mode to use instead.
pub fn install_start<S: SystemCtl>(
	sc: &S,
	opts: &StartUnitOpts,
	dry_run: bool,
	no_start: bool,
) -> crate::Result<()> {
	let project = &opts.project;
	refuse_if_quadlet_present(project)?;
	validate_start_unit_opts(opts).map_err(ComposeError::Autostart)?;
	let unit_text = render_start_unit(opts);
	place_unit(sc, project, &unit_text, dry_run, no_start)
}

/// Write the unit, reload systemd and (unless `no_start`) enable it. Shared by
/// every mode that writes one final `.service` file, so the two do not drift on
/// where the file lands or which systemctl calls follow it.
fn place_unit<S: SystemCtl>(
	sc: &S,
	project: &str,
	unit_text: &str,
	dry_run: bool,
	no_start: bool,
) -> crate::Result<()> {
	let path = unit_path(project);
	let unit_name = unit_file_name(project);

	// Surface the linger / session guards before acting (or previewing).
	emit_guards(sc);

	if dry_run {
		print!("{unit_text}");
		println!("\n# would write {}", path.display());
		println!("# would run: systemctl --user daemon-reload");
		if no_start {
			println!("# (--no-start) would not enable or start the unit");
		} else {
			println!("# would run: systemctl --user enable --now {unit_name}");
		}
		return Ok(());
	}

	let dir = unit_dir();
	std::fs::create_dir_all(&dir)
		.map_err(|e| ComposeError::Autostart(format!("cannot create {}: {e}", dir.display())))?;
	std::fs::write(&path, unit_text.as_bytes())
		.map_err(|e| ComposeError::Autostart(format!("cannot write {}: {e}", path.display())))?;
	eprintln!("podup: wrote {}", path.display());

	checked(sc.systemctl(&["daemon-reload"]), "daemon-reload")?;
	if no_start {
		eprintln!("podup: installed {unit_name} (not enabled; --no-start)");
	} else {
		checked(
			sc.systemctl(&["enable", "--now", &unit_name]),
			&format!("enable --now {unit_name}"),
		)?;
		eprintln!("podup: enabled and started {unit_name}");
	}
	Ok(())
}

/// Write the auto-update oneshot service and its timer, then enable the timer
/// (not the service, the timer is what the user enables; the service runs
/// only when the timer fires). `interval` is the `OnCalendar=` word
/// (`hourly`/`daily`/`weekly`). The service is installed but not `enable
/// --now`'d: nothing would start it that way, since the timer is the entry
/// point and a disabled timer means nothing fires.
fn place_update_units<S: SystemCtl>(
	sc: &S,
	project: &str,
	main_opts: &service::ServiceUnitOpts,
	interval: &str,
	dry_run: bool,
	no_start: bool,
) -> crate::Result<()> {
	let service_text = service::render_update_service_unit(main_opts);
	let timer_text = service::render_update_timer_unit(project, interval);
	let service_name = service::update_service_file_name(project);
	let timer_name = service::update_timer_file_name(project);
	let dir = unit_dir();
	let service_path = dir.join(&service_name);
	let timer_path = dir.join(&timer_name);

	if dry_run {
		print!("{service_text}");
		print!("{timer_text}");
		println!("\n# would write {}", service_path.display());
		println!("# would write {}", timer_path.display());
		println!("# would run: systemctl --user daemon-reload");
		if no_start {
			println!("# (--no-start) would not enable or start {service_name} or {timer_name}");
		} else {
			println!("# would run: systemctl --user enable --now {timer_name}");
		}
		return Ok(());
	}

	std::fs::write(&service_path, service_text.as_bytes()).map_err(|e| {
		ComposeError::Autostart(format!("cannot write {}: {e}", service_path.display()))
	})?;
	eprintln!("podup: wrote {}", service_path.display());
	std::fs::write(&timer_path, timer_text.as_bytes()).map_err(|e| {
		ComposeError::Autostart(format!("cannot write {}: {e}", timer_path.display()))
	})?;
	eprintln!("podup: wrote {}", timer_path.display());

	checked(sc.systemctl(&["daemon-reload"]), "daemon-reload")?;
	if !no_start {
		checked(
			sc.systemctl(&["enable", "--now", &timer_name]),
			&format!("enable --now {timer_name}"),
		)?;
		eprintln!("podup: enabled and started {timer_name}");
	} else {
		eprintln!("podup: installed {service_name} and {timer_name} (not enabled; --no-start)");
	}
	Ok(())
}

/// Whether systemd knows anything about `unit` — loaded, enabled, running, or
/// merely present as a fragment.
///
/// `systemctl is-active` exits **4** for a unit it has never heard of and
/// something else for every other state (0 active, 3 inactive/failed/activating).
/// That numeric 4 is the only reliable "there is nothing here" signal: the
/// message text is localised, and the *fragment file* is not a proxy for it —
/// measured, a unit whose file is deleted out of band stays loaded, enabled and
/// running, and `disable --now` still exits 0, removes its `.wants/` symlink and
/// stops it. Gating on the file would delete the only way out of that state.
///
/// A probe that cannot even be spawned returns `true`: the right response to
/// "I could not ask" is to attempt the disable anyway and let `checked` report
/// whatever happens, never to assume there is nothing to do.
fn unit_is_known<S: SystemCtl>(sc: &S, unit: &str) -> bool {
	sc.systemctl(&["is-active", "--quiet", unit])
		.map(|o| o.status.code() != Some(4))
		.unwrap_or(true)
}

/// Uninstall the service-mode autostart unit: disable + stop it, remove the unit
/// file, and reload the user manager.
///
/// Idempotent — uninstalling when nothing is installed is a quiet no-op — but a
/// `disable` that genuinely fails is reported rather than swallowed, so the
/// command cannot claim success while the service is still enabled and running.
///
/// When the auto-update timer pair is present, this removes both: the
/// `<unit>-update.service` oneshot and the `<unit>-update.timer` schedule that
/// fires it. Without that, the timer would keep firing `up -d` against a stack
/// whose main unit had been uninstalled, the exact inconsistency the brief
/// calls out.
pub fn uninstall<S: SystemCtl>(sc: &S, project: &str) -> crate::Result<()> {
	let unit_name = unit_file_name(project);
	let path = unit_path(project);

	// Disable whenever systemd knows the unit, whether or not its file is still
	// there. `disable --now` is idempotent across every state it can be in
	// (enabled, never enabled, running, stopped, fragment deleted out of band),
	// so the only case worth skipping is the one where systemd has never heard
	// of it — which is exactly what `unit_is_known` answers.
	if unit_is_known(sc, &unit_name) {
		checked(
			sc.systemctl(&["disable", "--now", &unit_name]),
			"disable --now",
		)?;
	}

	if path.exists() {
		std::fs::remove_file(&path).map_err(|e| {
			ComposeError::Autostart(format!("cannot remove {}: {e}", path.display()))
		})?;
		eprintln!("podup: removed {}", path.display());
	} else {
		eprintln!(
			"podup: no unit file at {} (already removed)",
			path.display()
		);
	}

	// The auto-update pair, when present. Run independently of the main unit's
	// outcome: the timer pair is its own install and uninstalls the same way.
	let update_service_name = service::update_service_file_name(project);
	let update_timer_name = service::update_timer_file_name(project);
	let update_service_path = unit_dir().join(&update_service_name);
	let update_timer_path = unit_dir().join(&update_timer_name);

	if unit_is_known(sc, &update_timer_name) {
		checked(
			sc.systemctl(&["disable", "--now", &update_timer_name]),
			&format!("disable --now {update_timer_name}"),
		)?;
	}
	if unit_is_known(sc, &update_service_name) {
		checked(
			sc.systemctl(&["disable", "--now", &update_service_name]),
			&format!("disable --now {update_service_name}"),
		)?;
	}
	for (_name, path) in [
		(update_service_name.as_str(), &update_service_path),
		(update_timer_name.as_str(), &update_timer_path),
	] {
		if path.exists() {
			std::fs::remove_file(path).map_err(|e| {
				ComposeError::Autostart(format!("cannot remove {}: {e}", path.display()))
			})?;
			eprintln!("podup: removed {}", path.display());
		}
	}

	checked(sc.systemctl(&["daemon-reload"]), "daemon-reload")?;
	Ok(())
}

/// Which autostart mode, if any, is installed for a project. Service and quadlet
/// mode cannot coexist — each install refuses the other — so at most one is present.
/// `uninstall` uses this to remove whichever is there without the caller naming a
/// mode (and mistakenly no-op'ing against the wrong one).
pub enum InstalledMode {
	/// The service-mode `podup-<project>.service` unit is present.
	Service,
	/// Quadlet `<project>-*.container` units are present.
	Quadlet,
	/// Neither — nothing is installed.
	None,
}

/// Detect the installed autostart mode for `project` from what is on disk.
pub fn installed_mode(project: &str) -> InstalledMode {
	if unit_path(project).exists() {
		InstalledMode::Service
	} else if !quadlet_units_present(project).is_empty() {
		InstalledMode::Quadlet
	} else {
		InstalledMode::None
	}
}

/// Whether the network-ordering the generated units declare is real.
///
/// `Wants=`/`After=` naming a unit systemd cannot load is dropped silently:
/// `LoadState` reads `not-found`, the dependent starts clean, and nothing
/// reaches the journal. So a unit file can say it waits for the network while
/// waiting for nothing, and the only way to tell is to ask.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkWait {
	/// systemd loaded the shim, so the ordering the unit declares takes effect.
	Loaded,
	/// No such unit. Podman ships it from 5.3.0; below that the ordering in
	/// every autostart unit is inert.
	NotFound,
	/// `systemctl` could not be asked, or answered something unrecognised.
	/// Carried rather than collapsed into `NotFound`: "we could not tell" and
	/// "it is not there" are different answers and only one is a problem.
	Unknown(String),
}

/// A snapshot of the autostart unit's state, gathered for `status`.
pub struct StatusReport {
	/// Absolute path to where the unit file would live.
	pub unit_path: PathBuf,
	/// Whether the unit file exists on disk.
	pub unit_exists: bool,
	/// The unit file's permission bits (Unix only), when it exists.
	pub unit_mode: Option<u32>,
	/// `systemctl --user is-active` output (e.g. `active`, `inactive`, `failed`).
	pub is_active: String,
	/// `systemctl --user is-enabled` output (e.g. `enabled`, `disabled`).
	pub is_enabled: String,
	/// Whether linger is enabled for the current user.
	pub linger: bool,
	/// Whether `XDG_RUNTIME_DIR` is set (a user session is present).
	pub runtime_dir: bool,
	/// Whether the network shim the generated units order against is loadable.
	pub network_wait: NetworkWait,
}

/// Read the unit file's permission bits, on Unix. Other platforms have no POSIX
/// mode, so this is always `None` there.
#[cfg(unix)]
fn file_mode(path: &Path) -> Option<u32> {
	use std::os::unix::fs::PermissionsExt;
	std::fs::metadata(path).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Option<u32> {
	None
}

/// Run a `systemctl --user` query that reports state through its stdout (e.g.
/// `is-active`, `is-enabled`); these exit non-zero for the negative answer, so
/// the trimmed stdout is the report regardless of exit status.
fn query<S: SystemCtl>(sc: &S, arg: &str, unit_name: &str) -> String {
	match sc.systemctl(&[arg, unit_name]) {
		Ok(out) => {
			let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
			if s.is_empty() {
				"unknown".to_string()
			} else {
				s
			}
		}
		Err(e) => format!("unknown ({e})"),
	}
}

/// The unit every autostart mode orders against, so the name lives in one place.
pub(crate) const NETWORK_SHIM: &str = "podman-user-wait-network-online.service";

/// Ask systemd whether the network shim is loadable.
///
/// **The exit code is not the signal, and must not be used as one.** Measured
/// 2026-08-30 on Podman 5.7.0, rootless: `systemctl --user show <unit> -p
/// LoadState` exits **0 for a unit that does not exist** just as it does for one
/// that does, printing `LoadState=not-found` in the first case and
/// `LoadState=loaded` in the second. A guard branching on the status would
/// report the shim as fine while it is missing, which is the same vacuous check
/// this function exists to replace.
///
/// That differs from the two verbs the rest of this status uses: `is-active`
/// and `is-enabled` both exit **4** for an unknown unit. Two conventions, one
/// module, so the difference is stated here rather than left to be rediscovered.
fn network_wait_state<S: SystemCtl>(sc: &S) -> NetworkWait {
	let out = match sc.systemctl(&["show", NETWORK_SHIM, "-p", "LoadState"]) {
		Ok(out) => out,
		Err(e) => return NetworkWait::Unknown(format!("systemctl show failed: {e}")),
	};
	let text = String::from_utf8_lossy(&out.stdout);
	let Some(state) = text
		.lines()
		.find_map(|l| l.trim().strip_prefix("LoadState="))
	else {
		return NetworkWait::Unknown("systemctl show printed no LoadState".to_string());
	};
	match state.trim() {
		"loaded" => NetworkWait::Loaded,
		"not-found" => NetworkWait::NotFound,
		other => NetworkWait::Unknown(other.to_string()),
	}
}

/// Gather the autostart status for a project, going through the [`SystemCtl`] seam
/// so it is testable without a live systemd.
pub fn collect_status<S: SystemCtl>(sc: &S, project: &str) -> StatusReport {
	let unit_name = unit_file_name(project);
	let path = unit_path(project);
	let unit_exists = path.exists();
	StatusReport {
		unit_mode: if unit_exists { file_mode(&path) } else { None },
		unit_exists,
		unit_path: path,
		is_active: query(sc, "is-active", &unit_name),
		is_enabled: query(sc, "is-enabled", &unit_name),
		linger: current_user().is_some_and(|u| linger_enabled(sc, &u)),
		runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|s| !s.is_empty()),
		network_wait: network_wait_state(sc),
	}
}

/// Print the autostart status for a project.
pub fn status<S: SystemCtl>(sc: &S, project: &str) -> crate::Result<()> {
	let r = collect_status(sc, project);
	// Every line here is a yes/no an operator is scanning for, and the whole
	// screen was one colour — so "is it actually running?" meant reading six
	// labels to find the one word that answers it. The label is scaffolding
	// (dimmed); the value carries the meaning (tinted by what it says).
	let row = |label: &str, value: &str| {
		crate::ui::print_labelled(label, value);
	};
	row("unit", &r.unit_path.display().to_string());
	row("installed", if r.unit_exists { "yes" } else { "no" });
	if let Some(mode) = r.unit_mode {
		row("mode", &format!("{:04o}", mode & 0o7777));
	}
	// With no unit file on disk, systemd's answers to both questions are the same
	// negative the `installed: no` line above already gave, and neither is a
	// failure — but `is-enabled` returns the word `not-found`, which the status
	// vocabulary reads as an error and paints red. So an uninstalled project
	// reported one dim `no` and one red `not-found` about the same unit.
	//
	// A unit that *is* on disk while systemd still cannot find it stays red: that
	// is a genuinely broken install, and the two cases must not look alike.
	if r.unit_exists {
		row("active", &r.is_active);
		row("enabled", &r.is_enabled);
	} else {
		crate::ui::print_labelled_neutral("active", &r.is_active);
		crate::ui::print_labelled_neutral("enabled", &r.is_enabled);
	}
	row("linger", if r.linger { "enabled" } else { "disabled" });
	// Prose for the same reason the session line is prose: "not-found" alone
	// would read as a broken install rather than as the ordering being inert,
	// and the operator needs to be told what it costs and what fixes it.
	match &r.network_wait {
		NetworkWait::Loaded => crate::ui::print_labelled_with(
			"network wait",
			&format!("{NETWORK_SHIM} is loaded"),
			Some(true),
		),
		NetworkWait::NotFound => crate::ui::print_labelled_with(
			"network wait",
			&format!(
				"{NETWORK_SHIM} is not loadable, so the unit's network ordering \
				 is dropped silently (Podman ships it from 5.3.0)"
			),
			Some(false),
		),
		NetworkWait::Unknown(why) => {
			crate::ui::print_labelled_neutral("network wait", &format!("unknown ({why})"))
		}
	}
	// Prose, not a state word, so the meaning is stated rather than inferred.
	crate::ui::print_labelled_with(
		"session",
		if r.runtime_dir {
			"XDG_RUNTIME_DIR set"
		} else {
			"XDG_RUNTIME_DIR unset (systemctl --user needs a user session)"
		},
		Some(r.runtime_dir),
	);
	Ok(())
}

#[cfg(all(test, unix))]
mod tests;
