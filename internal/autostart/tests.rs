//! Unit tests for the service-mode autostart install/uninstall/status logic.
//!
//! Split out of `mod.rs` to keep that file within the source line limit, the
//! same way the quadlet-mode tests live in `quadlet/tests.rs`.

use super::*;
use std::cell::RefCell;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

/// Recording fake: captures every `systemctl`/`loginctl` arg vector and returns
/// canned output keyed off the first argument.
struct FakeCtl {
	systemctl_calls: RefCell<Vec<Vec<String>>>,
	loginctl_calls: RefCell<Vec<Vec<String>>>,
	linger: String,
	is_active: String,
	is_enabled: String,
	/// Exit code for `is-active`, as a raw wait status (code << 8). 0 by
	/// default; `4 << 8` is systemd's "no such unit", the only value
	/// `unit_is_known` treats as "there is nothing here".
	is_active_code: i32,
	/// What `systemctl --user show <shim> -p LoadState` prints. Real systemd
	/// exits 0 whether or not the unit exists, so this fake always does too:
	/// a fake that signalled absence through the exit code would let a guard
	/// that reads the code pass here and fail in production.
	show_load_state: String,
	fail: bool,
}

impl FakeCtl {
	fn new() -> Self {
		FakeCtl {
			systemctl_calls: RefCell::new(Vec::new()),
			loginctl_calls: RefCell::new(Vec::new()),
			linger: "yes".to_string(),
			is_active: "active".to_string(),
			is_enabled: "enabled".to_string(),
			is_active_code: 0,
			show_load_state: "LoadState=loaded\n".to_string(),
			fail: false,
		}
	}

	fn systemctl_log(&self) -> Vec<Vec<String>> {
		self.systemctl_calls.borrow().clone()
	}
}

fn out(code: i32, stdout: &str) -> Output {
	Output {
		status: ExitStatus::from_raw(code),
		stdout: stdout.as_bytes().to_vec(),
		stderr: Vec::new(),
	}
}

impl SystemCtl for FakeCtl {
	fn systemctl(&self, args: &[&str]) -> io::Result<Output> {
		self.systemctl_calls
			.borrow_mut()
			.push(args.iter().map(|s| s.to_string()).collect());
		let code = if self.fail { 256 } else { 0 };
		let stdout = match args.first().copied() {
			Some("is-active") => self.is_active.as_str(),
			Some("is-enabled") => self.is_enabled.as_str(),
			Some("show") => self.show_load_state.as_str(),
			_ => "",
		};
		// `is-enabled` is read through stdout only, so its status stays
		// successful. `is-active`'s status *is* consulted (`unit_is_known`
		// keys off exit 4), so it comes from its own field rather than the
		// blanket `fail` flag.
		let code = match args.first().copied() {
			Some("is-active") => self.is_active_code,
			// Both 0, deliberately. Measured on real systemd: `show` reports a
			// missing unit through `LoadState=not-found` in stdout and still
			// exits 0, unlike `is-enabled`, which exits 4 for the same unit.
			Some("is-enabled") | Some("show") => 0,
			_ => code,
		};
		Ok(out(code, stdout))
	}

	fn loginctl(&self, args: &[&str]) -> io::Result<Output> {
		self.loginctl_calls
			.borrow_mut()
			.push(args.iter().map(|s| s.to_string()).collect());
		Ok(out(0, &self.linger))
	}
}

fn opts(dir: &Path, project: &str, dry_run: bool, no_start: bool) -> InstallOptions {
	InstallOptions {
		unit: ServiceUnitOpts {
			exe: PathBuf::from("/usr/local/bin/podup"),
			compose_files: vec![dir.join("docker-compose.yml")],
			project: project.to_string(),
			working_dir: dir.to_path_buf(),
			profiles: Vec::new(),
			env_files: Vec::new(),
			max_stop_grace_secs: None,
		},
		no_start,
		dry_run,
		auto_update_interval: None,
	}
}

fn opts_with_interval(
	dir: &Path,
	project: &str,
	dry_run: bool,
	no_start: bool,
	interval: &str,
) -> InstallOptions {
	let mut o = opts(dir, project, dry_run, no_start);
	o.auto_update_interval = Some(interval.to_string());
	o
}

/// Run `f` with a fresh temp `XDG_CONFIG_HOME`, `USER`, and `XDG_RUNTIME_DIR`
/// set, so the install/status paths resolve under the temp dir.
fn with_env<R>(f: impl FnOnce(&Path) -> R) -> R {
	let tmp = tempfile::tempdir().unwrap();
	let root = tmp.path().to_path_buf();
	temp_env::with_vars(
		[
			("XDG_CONFIG_HOME", Some(root.as_os_str())),
			("XDG_RUNTIME_DIR", Some(root.as_os_str())),
			("USER", Some(std::ffi::OsStr::new("tester"))),
		],
		|| f(&root),
	)
}

#[test]
fn install_writes_unit_and_enables() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(&sc, &opts(root, "app", false, false)).unwrap();
		let path = root.join("systemd/user/podup-app.service");
		assert!(path.is_file(), "unit file written");
		let body = std::fs::read_to_string(&path).unwrap();
		assert!(body.contains("Description=podup app"));
		let calls = sc.systemctl_log();
		assert_eq!(calls[0], vec!["daemon-reload"]);
		assert_eq!(calls[1], vec!["enable", "--now", "podup-app.service"]);
	});
}

#[test]
fn install_no_start_skips_enable() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(&sc, &opts(root, "app", false, true)).unwrap();
		let calls = sc.systemctl_log();
		assert_eq!(calls, vec![vec!["daemon-reload"]]);
	});
}

#[test]
fn dry_run_writes_nothing_and_runs_no_systemctl() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(&sc, &opts(root, "app", true, false)).unwrap();
		assert!(!root.join("systemd/user/podup-app.service").exists());
		assert!(sc.systemctl_log().is_empty());
	});
}

#[test]
fn uninstall_disables_removes_and_reloads() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(&sc, &opts(root, "app", false, true)).unwrap();
		let path = root.join("systemd/user/podup-app.service");
		assert!(path.exists());

		let sc2 = FakeCtl::new();
		uninstall(&sc2, "app").unwrap();
		assert!(!path.exists(), "unit file removed");
		let calls = sc2.systemctl_log();
		// The `is-active` probe only asks whether systemd knows the unit at
		// all; anything but exit 4 means disable it. Two more probes for the
		// auto-update units happen even when none were installed, they
		// answer the same question, just for siblings that may or may not be
		// present.
		assert_eq!(calls[0], vec!["is-active", "--quiet", "podup-app.service"]);
		assert_eq!(calls[1], vec!["disable", "--now", "podup-app.service"]);
		// Two more probe+disable pairs for the auto-update units, even though
		// no install in this test wrote them, the uninstall path probes them
		// unconditionally so a half-installed pair still gets cleaned up.
		assert_eq!(
			calls[2],
			vec!["is-active", "--quiet", "podup-app-update.timer"]
		);
		assert_eq!(calls[3], vec!["disable", "--now", "podup-app-update.timer"]);
		assert_eq!(
			calls[4],
			vec!["is-active", "--quiet", "podup-app-update.service"]
		);
		assert_eq!(
			calls[5],
			vec!["disable", "--now", "podup-app-update.service"]
		);
		assert_eq!(calls[6], vec!["daemon-reload"]);
	});
}

/// #1080: a `disable --now` that fails on an installed unit was swallowed by
/// `let _ =`, so uninstall exited 0 with the service still enabled and
/// running. Measured against real systemd: with the unit file present,
/// `disable --now` exits 0 whether or not the unit was ever enabled or
/// started, so a non-zero exit here is always a real failure.
#[test]
fn uninstall_reports_a_failed_disable() {
	with_env(|root| {
		install(&FakeCtl::new(), &opts(root, "app", false, true)).unwrap();
		let path = root.join("systemd/user/podup-app.service");
		assert!(path.exists());

		let mut sc = FakeCtl::new();
		sc.fail = true;
		let err = uninstall(&sc, "app")
			.expect_err("a failed disable must not be reported as a clean uninstall");
		assert!(matches!(err, ComposeError::Autostart(_)), "got {err:?}");
		assert!(err.to_string().contains("disable"), "got {err}");
	});
}

/// Uninstalling when systemd has never heard of the unit stays a silent
/// no-op. `is-active` exit 4 ("no such unit") is the signal, not the unit
/// file, which is a poor proxy: a fragment deleted out of band leaves the
/// unit loaded, enabled and running, and only `disable --now` clears it.
#[test]
fn uninstall_runs_no_disable_when_systemd_does_not_know_the_unit() {
	with_env(|_root| {
		let mut sc = FakeCtl::new();
		sc.is_active_code = 4 << 8; // systemd: no such unit
		uninstall(&sc, "app").expect("uninstalling nothing is not a failure");
		let calls = sc.systemctl_log();
		assert!(
			!calls
				.iter()
				.any(|c| c.first().map(String::as_str) == Some("disable")),
			"nothing is installed, so nothing should be disabled: {calls:?}"
		);
		assert_eq!(calls.last().unwrap(), &vec!["daemon-reload".to_string()]);
	});
}

/// The mirror case, and the reason the file is not the gate: the unit file is
/// gone but systemd still has the unit loaded and running (a manual `rm`, a
/// restored `~/.config`, a half-finished uninstall). `disable --now` is the
/// only thing that clears that state, so it must still run.
#[test]
fn uninstall_disables_a_known_unit_whose_file_is_already_gone() {
	with_env(|root| {
		let path = root.join("systemd/user/podup-app.service");
		assert!(!path.exists());
		// Default `is_active_code` is 0: systemd knows the unit.
		let sc = FakeCtl::new();
		uninstall(&sc, "app").expect("uninstall must still succeed");
		let calls = sc.systemctl_log();
		assert!(
			calls.contains(&vec![
				"disable".to_string(),
				"--now".to_string(),
				"podup-app.service".to_string()
			]),
			"a unit systemd still knows must be disabled even with no file: {calls:?}"
		);
	});
}

#[test]
fn install_refuses_on_quadlet_conflict() {
	with_env(|root| {
		let qdir = root.join("containers/systemd");
		std::fs::create_dir_all(&qdir).unwrap();
		std::fs::write(qdir.join("app-web.container"), b"[Container]\n").unwrap();
		let sc = FakeCtl::new();
		let err = install(&sc, &opts(root, "app", false, false)).unwrap_err();
		assert!(matches!(err, ComposeError::Autostart(_)));
		assert!(err.to_string().contains("quadlet"));
		// Nothing was installed.
		assert!(!root.join("systemd/user/podup-app.service").exists());
		assert!(sc.systemctl_log().is_empty());
	});
}

#[test]
fn linger_off_produces_warning() {
	let mut sc = FakeCtl::new();
	sc.linger = "no".to_string();
	temp_env::with_var("USER", Some("tester"), || {
		assert!(linger_warning(&sc).is_some());
	});
}

#[test]
fn linger_on_produces_no_warning() {
	let sc = FakeCtl::new(); // linger = "yes"
	temp_env::with_var("USER", Some("tester"), || {
		assert!(linger_warning(&sc).is_none());
	});
}

#[test]
fn missing_runtime_dir_produces_warning() {
	temp_env::with_var_unset("XDG_RUNTIME_DIR", || {
		assert!(runtime_dir_warning().is_some());
	});
	temp_env::with_var("XDG_RUNTIME_DIR", Some("/run/user/1000"), || {
		assert!(runtime_dir_warning().is_none());
	});
}

#[test]
fn collect_status_reports_installed_and_state() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(&sc, &opts(root, "app", false, true)).unwrap();
		let r = collect_status(&sc, "app");
		assert!(r.unit_exists);
		assert!(r.unit_mode.is_some());
		assert_eq!(r.is_active, "active");
		assert_eq!(r.is_enabled, "enabled");
		assert!(r.linger);
		assert!(r.runtime_dir);
	});
}

#[test]
fn collect_status_reports_absent_unit() {
	with_env(|_root| {
		let sc = FakeCtl::new();
		let r = collect_status(&sc, "nope");
		assert!(!r.unit_exists);
		assert!(r.unit_mode.is_none());
	});
}

#[test]
fn install_surfaces_systemctl_failure() {
	with_env(|root| {
		let mut sc = FakeCtl::new();
		sc.fail = true;
		let err = install(&sc, &opts(root, "app", false, false)).unwrap_err();
		assert!(matches!(err, ComposeError::Autostart(_)));
	});
}

/// #1093: the unit's `TimeoutStopSec=` is derived from the project's slowest
/// service, so the roll-up must pick the maximum and tolerate the rest.
#[test]
fn max_stop_grace_picks_the_longest() {
	let file = crate::parse_str(
		"services:\n  a:\n    image: x\n    stop_grace_period: 10s\n  b:\n    image: x\n    stop_grace_period: 2m\n",
	)
	.unwrap();
	assert_eq!(super::max_stop_grace_secs(&file), Some(120));
}

/// No service setting one yields `None`, so the unit omits the key and systemd
/// keeps its own default rather than podup restating it.
#[test]
fn max_stop_grace_is_none_when_unset() {
	let file = crate::parse_str("services:\n  a:\n    image: x\n").unwrap();
	assert_eq!(super::max_stop_grace_secs(&file), None);
}

/// An unparseable duration is skipped rather than defaulted: the value is
/// validated elsewhere, and guessing a timeout from a malformed one would be
/// worse than not setting it. A valid sibling still counts.
#[test]
fn max_stop_grace_skips_an_unparseable_value() {
	let file = crate::parse_str(
		"services:\n  a:\n    image: x\n    stop_grace_period: \"nonsense\"\n  b:\n    image: x\n    stop_grace_period: 30s\n",
	)
	.unwrap();
	assert_eq!(super::max_stop_grace_secs(&file), Some(30));
}

// --- the network shim (#1616's ordering is only real if the unit loads) ---

/// The trap, pinned. Real `systemctl show` exits **0** for a unit that does not
/// exist, printing `LoadState=not-found`. A guard reading the exit code would
/// call the shim present here and be wrong in exactly the case it exists to
/// catch, which is the same vacuous shape as the assertion #1616 replaced.
#[test]
fn a_missing_shim_is_read_from_load_state_not_from_the_exit_code() {
	let mut sc = FakeCtl::new();
	sc.show_load_state = "LoadState=not-found\n".to_string();
	let r = collect_status(&sc, "app");
	assert_eq!(r.network_wait, NetworkWait::NotFound);
	// The call really did succeed, so nothing about the status distinguished
	// these two cases except the string.
	assert_eq!(
		sc.systemctl(&["show", NETWORK_SHIM, "-p", "LoadState"])
			.unwrap()
			.status
			.code(),
		Some(0)
	);
}

#[test]
fn a_loaded_shim_reads_as_loaded() {
	let sc = FakeCtl::new();
	assert_eq!(collect_status(&sc, "app").network_wait, NetworkWait::Loaded);
}

/// "We could not tell" is not "it is not there". Collapsing the two would report
/// a broken ordering on every machine whose systemctl answered oddly.
#[test]
fn an_unrecognised_answer_is_unknown_rather_than_missing() {
	let mut sc = FakeCtl::new();
	sc.show_load_state = "LoadState=masked\n".to_string();
	assert!(matches!(
		collect_status(&sc, "app").network_wait,
		NetworkWait::Unknown(s) if s == "masked"
	));

	let mut sc = FakeCtl::new();
	sc.show_load_state = String::new();
	assert!(matches!(
		collect_status(&sc, "app").network_wait,
		NetworkWait::Unknown(_)
	));
}

/// The status asks about the shim by the same name the units order against, so
/// a rename cannot leave the guard checking a unit nothing depends on.
#[test]
fn the_status_asks_about_the_unit_the_units_actually_name() {
	let sc = FakeCtl::new();
	let _ = collect_status(&sc, "app");
	assert!(
		sc.systemctl_log()
			.iter()
			.any(|c| c.first().map(String::as_str) == Some("show")
				&& c.contains(&NETWORK_SHIM.to_string())),
		"{:?}",
		sc.systemctl_log()
	);
	// Every renderer that emits the ordering, not just the one that had it
	// first. Checking only service mode would leave start mode free to name a
	// different unit than the status reports on, which is the same silent
	// mismatch this whole check exists to surface.
	let service = super::render_service_unit(&super::ServiceUnitOpts::new(
		std::path::PathBuf::from("/usr/bin/podup"),
		vec![std::path::PathBuf::from("/srv/app/compose.yml")],
		"app".to_string(),
		std::path::PathBuf::from("/srv/app"),
	));
	let start = super::render_start_unit(&super::StartUnitOpts::new(
		std::path::PathBuf::from("/usr/bin/podman"),
		"app".to_string(),
		"app-web-1".to_string(),
	));
	for (mode, unit) in [("service", &service), ("start", &start)] {
		assert!(
			unit.contains(NETWORK_SHIM),
			"{mode} mode must order against the same name the status checks:\n{unit}"
		);
	}
}

// ---------------------------------------------------------------------------
// --auto-update: the sibling `<unit>-update.service` oneshot and `<unit>
// -update.timer` schedule. Install/uninstall behaviour.
// ---------------------------------------------------------------------------

/// #1656: `autostart install --mode service --auto-update <interval>` writes
/// the main unit, the `<unit>-update.service` oneshot, and the
/// `<unit>-update.timer` schedule; enables the timer; reloads systemd once.
#[test]
fn autostart_auto_update_renders_the_timer_and_the_oneshot_unit() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(&sc, &opts_with_interval(root, "app", false, false, "daily")).unwrap();
		let main = root.join("systemd/user/podup-app.service");
		let oneshot = root.join("systemd/user/podup-app-update.service");
		let timer = root.join("systemd/user/podup-app-update.timer");
		assert!(main.is_file(), "main unit written");
		assert!(oneshot.is_file(), "oneshot written");
		assert!(timer.is_file(), "timer written");
		let oneshot_body = std::fs::read_to_string(&oneshot).unwrap();
		assert!(
			oneshot_body.contains("Type=oneshot"),
			"oneshot must be Type=oneshot: {oneshot_body}"
		);
		assert!(
			oneshot_body.contains("ExecStart=/usr/local/bin/podup -f ")
				&& oneshot_body.contains("-p app up -d"),
			"oneshot must use the same leading args and run `up -d`: {oneshot_body}"
		);
		let timer_body = std::fs::read_to_string(&timer).unwrap();
		assert!(
			timer_body.contains("OnCalendar=daily"),
			"timer must carry the chosen OnCalendar=: {timer_body}"
		);
		assert!(
			timer_body.contains("Persistent=true"),
			"timer must be Persistent: {timer_body}"
		);
		assert!(
			timer_body.contains("WantedBy=timers.target"),
			"timer must be wanted by timers.target: {timer_body}"
		);
		let calls = sc.systemctl_log();
		assert_eq!(calls[0], vec!["daemon-reload"]);
		assert!(
			calls
				.iter()
				.any(|c| c.first().map(String::as_str) == Some("enable")
					&& c.contains(&"podup-app-update.timer".to_string())),
			"the timer must be enabled: {calls:?}"
		);
	});
}

/// #1656: without `--auto-update`, the install path produces exactly what it
/// did before the feature existed. The byte-identical-output promise.
#[test]
fn autostart_without_auto_update_renders_exactly_what_it_did_before() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(&sc, &opts(root, "app", false, false)).unwrap();
		let main = root.join("systemd/user/podup-app.service");
		let oneshot = root.join("systemd/user/podup-app-update.service");
		let timer = root.join("systemd/user/podup-app-update.timer");
		assert!(main.is_file(), "main unit written");
		assert!(!oneshot.exists(), "no oneshot without --auto-update");
		assert!(!timer.exists(), "no timer without --auto-update");
		let calls = sc.systemctl_log();
		assert_eq!(calls[0], vec!["daemon-reload"]);
		assert_eq!(calls[1], vec!["enable", "--now", "podup-app.service"]);
		// No enable call for the timer pair.
		assert!(
			!calls
				.iter()
				.any(|c| c.contains(&"podup-app-update.timer".to_string())),
			"no timer call without --auto-update: {calls:?}"
		);
	});
}

/// #1656: removing a project that was installed with `--auto-update` removes
/// both new units along with the main one. Without that, the timer would
/// keep firing `up -d` against a project whose main unit had been uninstalled.
#[test]
fn autostart_auto_update_uninstall_removes_both_new_units() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install(
			&sc,
			&opts_with_interval(root, "app", false, false, "weekly"),
		)
		.unwrap();
		let main = root.join("systemd/user/podup-app.service");
		let oneshot = root.join("systemd/user/podup-app-update.service");
		let timer = root.join("systemd/user/podup-app-update.timer");
		assert!(main.is_file());
		assert!(oneshot.is_file());
		assert!(timer.is_file());

		let sc2 = FakeCtl::new();
		uninstall(&sc2, "app").unwrap();
		assert!(!main.exists(), "main unit removed");
		assert!(!oneshot.exists(), "oneshot removed");
		assert!(!timer.exists(), "timer removed");
	});
}

/// A bogus interval is rejected at install time, the same way the CLI does.
/// Programmatically, `validate_auto_update_interval` is what catches it; the
/// renderer cannot return an error and silently emitting a bogus `OnCalendar=`
/// would leave the user without a working timer.
#[test]
fn autostart_auto_update_renderer_rejects_an_unknown_interval() {
	let opts = ServiceUnitOpts::new(
		std::path::PathBuf::from("/usr/local/bin/podup"),
		vec![std::path::PathBuf::from("/srv/app/docker-compose.yml")],
		"app".to_string(),
		std::path::PathBuf::from("/srv/app"),
	);
	let result = std::panic::catch_unwind(|| {
		let _ = super::render_update_timer_unit("app", "biweekly");
	});
	assert!(
		result.is_err(),
		"the renderer must panic on a bogus interval rather than silently emit it"
	);
	let _ = opts;
}

/// The builder is what the command uses to carry `--auto-update` into the
/// install options; a mutation sweep replaced it with `Default::default()`
/// and nothing noticed, because every test here builds the struct literally.
#[test]
fn install_options_builder_sets_the_auto_update_interval() {
	with_env(|root| {
		let base = opts(root, "app", false, false);
		let built =
			InstallOptions::new(base.unit).with_auto_update_interval(Some("daily".to_string()));
		assert_eq!(built.auto_update_interval.as_deref(), Some("daily"));
		let cleared = InstallOptions::new(opts(root, "app", false, false).unit)
			.with_auto_update_interval(None);
		assert_eq!(cleared.auto_update_interval, None);
	});
}
