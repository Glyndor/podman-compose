use super::{container_services, install_quadlet, rebuild_quadlet, uninstall_quadlet};
use crate::autostart::SystemCtl;
use crate::{parse_str, quadlet};
use std::cell::RefCell;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output};

/// Records every `systemctl` arg vector; `loginctl` always reports linger on.
struct FakeCtl {
	calls: RefCell<Vec<Vec<String>>>,
}
impl FakeCtl {
	fn new() -> Self {
		FakeCtl {
			calls: RefCell::new(Vec::new()),
		}
	}
	fn log(&self) -> Vec<Vec<String>> {
		self.calls.borrow().clone()
	}
}
impl SystemCtl for FakeCtl {
	fn systemctl(&self, args: &[&str]) -> std::io::Result<Output> {
		self.calls
			.borrow_mut()
			.push(args.iter().map(|s| s.to_string()).collect());
		Ok(Output {
			status: ExitStatus::from_raw(0),
			stdout: Vec::new(),
			stderr: Vec::new(),
		})
	}
	fn loginctl(&self, _args: &[&str]) -> std::io::Result<Output> {
		Ok(Output {
			status: ExitStatus::from_raw(0),
			stdout: b"yes".to_vec(),
			stderr: Vec::new(),
		})
	}
}

/// A [`FakeCtl`] whose exit status is decided per call, so a test can make one
/// specific `systemctl` verb fail while the rest succeed. `FakeCtl` always
/// reports 0, which cannot express "this service is not running" (`is-active`
/// non-zero) or "the stop was refused".
struct ScriptedCtl {
	calls: RefCell<Vec<Vec<String>>>,
	/// Exit code for a given arg vector; 0 means success.
	code: ExitCodeFor,
}

/// Decides a scripted `systemctl` call's exit code from its arguments.
type ExitCodeFor = Box<dyn Fn(&[&str]) -> i32>;
impl ScriptedCtl {
	fn new(code: impl Fn(&[&str]) -> i32 + 'static) -> Self {
		ScriptedCtl {
			calls: RefCell::new(Vec::new()),
			code: Box::new(code),
		}
	}
	fn log(&self) -> Vec<Vec<String>> {
		self.calls.borrow().clone()
	}
}
impl SystemCtl for ScriptedCtl {
	fn systemctl(&self, args: &[&str]) -> std::io::Result<Output> {
		self.calls
			.borrow_mut()
			.push(args.iter().map(|s| s.to_string()).collect());
		Ok(Output {
			status: ExitStatus::from_raw((self.code)(args) << 8),
			stdout: Vec::new(),
			stderr: b"scripted failure".to_vec(),
		})
	}
	fn loginctl(&self, _args: &[&str]) -> std::io::Result<Output> {
		Ok(Output {
			status: ExitStatus::from_raw(0),
			stdout: b"yes".to_vec(),
			stderr: Vec::new(),
		})
	}
}

/// Run `f` with a fresh temp `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`/`USER`, so
/// `quadlet_dir` and the guards resolve under the temp dir.
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

const IMG: &str = "services:\n  web:\n    image: nginx\n";
const BUILD: &str = "services:\n  web:\n    build: .\n";
const BASE: &str = "/srv/app";

#[test]
fn container_services_names_only_containers() {
	let file = parse_str(IMG).unwrap();
	let units = quadlet::generate_at(&file, "proj", Path::new(BASE)).units;
	// The default network unit is present too, but only `.container`s become services.
	assert_eq!(
		container_services(&units),
		vec!["proj-web.service".to_string()]
	);
}

#[test]
fn install_writes_units_reloads_then_starts() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install_quadlet(
			&sc,
			&parse_str(IMG).unwrap(),
			"proj",
			Path::new(BASE),
			false,
			false,
		)
		.unwrap();
		assert!(root.join("containers/systemd/proj-web.container").is_file());
		let calls = sc.log();
		assert_eq!(calls[0], vec!["daemon-reload"]);
		assert_eq!(calls[1], vec!["start", "proj-web.service"]);
	});
}

#[test]
fn no_start_reloads_but_starts_nothing() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install_quadlet(
			&sc,
			&parse_str(IMG).unwrap(),
			"proj",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		assert!(root.join("containers/systemd/proj-web.container").is_file());
		assert_eq!(sc.log(), vec![vec!["daemon-reload".to_string()]]);
	});
}

#[test]
fn dry_run_writes_nothing_and_runs_no_systemctl() {
	with_env(|root| {
		let sc = FakeCtl::new();
		install_quadlet(
			&sc,
			&parse_str(IMG).unwrap(),
			"proj",
			Path::new(BASE),
			false,
			true,
		)
		.unwrap();
		assert!(!root.join("containers/systemd/proj-web.container").exists());
		assert!(sc.log().is_empty());
	});
}

#[test]
fn install_refuses_when_service_mode_is_present() {
	with_env(|root| {
		let sd = root.join("systemd/user");
		std::fs::create_dir_all(&sd).unwrap();
		std::fs::write(sd.join("podup-proj.service"), "x").unwrap();
		let err = install_quadlet(
			&FakeCtl::new(),
			&parse_str(IMG).unwrap(),
			"proj",
			Path::new(BASE),
			false,
			false,
		)
		.unwrap_err();
		assert!(format!("{err}").contains("service-mode autostart unit"));
	});
}

#[test]
fn uninstall_stops_removes_and_reloads() {
	with_env(|root| {
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(IMG).unwrap(),
			"proj",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		let sc = FakeCtl::new();
		uninstall_quadlet(&sc, "proj").unwrap();
		assert!(!root.join("containers/systemd/proj-web.container").exists());
		let calls = sc.log();
		assert!(calls.contains(&vec!["stop".to_string(), "proj-web.service".to_string()]));
		assert_eq!(calls.last().unwrap(), &vec!["daemon-reload".to_string()]);
	});
}

/// The stop is unconditional, and must stay that way.
///
/// An earlier version probed `is-active` first and skipped the stop when it
/// failed. Measured against real systemd, that is wrong twice over: `stop` on a
/// unit whose fragment is on disk exits 0 in *every* state (running, inactive,
/// never started), so the probe bought nothing, and a service still coming up
/// reports `activating`, which `is-active` calls a failure, so a stack caught
/// mid-start would have been skipped and left running while its units were
/// deleted.
///
/// Here `is-active` would report "not active"; the stop must be issued anyway.
#[test]
fn uninstall_stops_unconditionally_without_probing_is_active() {
	with_env(|_root| {
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(IMG).unwrap(),
			"proj",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		// Anything that asks `is-active` gets "not active"; everything else succeeds.
		let sc = ScriptedCtl::new(|args| i32::from(args.first() == Some(&"is-active")) * 3);
		uninstall_quadlet(&sc, "proj").expect("stopping an inactive unit is not a failure");
		let calls = sc.log();
		assert!(
			calls.contains(&vec!["stop".to_string(), "proj-web.service".to_string()]),
			"the stop must be issued regardless of is-active: {calls:?}"
		);
		assert!(
			!calls
				.iter()
				.any(|c| c.first().map(String::as_str) == Some("is-active")),
			"uninstall must not probe is-active at all: {calls:?}"
		);
	});
}

/// #1080: a running service that refuses to stop was swallowed by `let _ =`, so
/// uninstall exited 0 with the container still up. It must now report, while
/// still removing the unit files and reloading, so the uninstall is not left
/// half-done.
#[test]
fn uninstall_reports_a_stop_failure_but_still_removes_and_reloads() {
	with_env(|root| {
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(IMG).unwrap(),
			"proj",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		// Running (`is-active` 0), but the stop is refused.
		let sc = ScriptedCtl::new(|args| i32::from(args.first() == Some(&"stop")));
		let err = uninstall_quadlet(&sc, "proj")
			.expect_err("a refused stop must not be reported as a clean uninstall");
		assert!(format!("{err}").contains("stop"), "got {err:?}");

		assert!(
			!root.join("containers/systemd/proj-web.container").exists(),
			"the unit file must still be removed"
		);
		let calls = sc.log();
		assert_eq!(
			calls.last().unwrap(),
			&vec!["daemon-reload".to_string()],
			"the manager must still be reloaded: {calls:?}"
		);
	});
}

// --- bug: uninstall matched units by filename prefix alone, so
// `uninstall -p app` also matched (and deleted) sibling project
// `app-extra`'s units. installed_units must scope by the ownership marker
// instead. ---

#[test]
fn uninstall_scoped_by_ownership_label_leaves_sibling_project_untouched() {
	with_env(|root| {
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(IMG).unwrap(),
			"app",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(IMG).unwrap(),
			"app-extra",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		let sc = FakeCtl::new();
		uninstall_quadlet(&sc, "app").unwrap();
		// Only 'app's own unit is gone...
		assert!(!root.join("containers/systemd/app-web.container").exists());
		// ...the sibling 'app-extra', whose file name shares the `app-` prefix,
		// must survive untouched.
		assert!(root
			.join("containers/systemd/app-extra-web.container")
			.is_file());
		let calls = sc.log();
		assert!(calls.contains(&vec!["stop".to_string(), "app-web.service".to_string()]));
		assert!(!calls.contains(&vec![
			"stop".to_string(),
			"app-extra-web.service".to_string()
		]));
	});
}

// --- bug (deeper than the one above): a compose file's own `labels:` are
// rendered into the same `[Section]` as the trusted `Label=podup.project=`
// stamp, in the identical `Label=key=value` shape. A service under
// `app-extra` declaring `labels: {podup.project: app}` therefore produces a
// unit whose FIRST `Label=podup.project=` line reads `app` (forged) and
// whose SECOND reads `app-extra` (the real stamp). A scope check that reads
// the first `Label=podup.project=` match (the old behaviour) resolves this
// unit's owner as `app`, so `uninstall -p app` deletes a sibling project's
// unit again, through a different door than the filename-prefix bug above.
// The `# podup-owner:` marker closes this because compose `labels:` can
// never render as a `#`-prefixed line, so it cannot be pre-empted the same
// way. This reproduces against the label-based check and must keep passing
// against the marker-based one. ---

#[test]
fn uninstall_ignores_forged_project_label_from_sibling_compose_file() {
	with_env(|root| {
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(IMG).unwrap(),
			"app",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		// `app-extra`'s own service labels forge a `Label=podup.project=app`
		// line ahead of its real `Label=podup.project=app-extra` stamp, the
		// exact shape a hostile or careless compose file can produce.
		let forged = "services:\n  web:\n    image: nginx\n    labels:\n      podup.project: app\n";
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(forged).unwrap(),
			"app-extra",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		let unit_path = root.join("containers/systemd/app-extra-web.container");
		let contents = std::fs::read_to_string(&unit_path).unwrap();
		// Confirm the fixture actually reproduces the forgery: the forged
		// `Label=podup.project=app` line precedes the real
		// `Label=podup.project=app-extra` stamp.
		let forged_at = contents.find("Label=podup.project=app\n").unwrap();
		let real_at = contents.find("Label=podup.project=app-extra\n").unwrap();
		assert!(
			forged_at < real_at,
			"fixture did not reproduce the forged label ordering:\n{contents}"
		);

		let sc = FakeCtl::new();
		uninstall_quadlet(&sc, "app").unwrap();
		// 'app's own unit is gone...
		assert!(!root.join("containers/systemd/app-web.container").exists());
		// ...but 'app-extra's unit must survive: its `# podup-owner:` marker
		// (unforgeable) says `app-extra`, regardless of what its forged
		// `Label=podup.project=` line claims.
		assert!(
			unit_path.is_file(),
			"forged Label=podup.project=app caused app-extra's unit to be deleted by `uninstall -p app`"
		);
		let calls = sc.log();
		assert!(calls.contains(&vec!["stop".to_string(), "app-web.service".to_string()]));
		assert!(!calls.contains(&vec![
			"stop".to_string(),
			"app-extra-web.service".to_string()
		]));
	});
}

#[test]
fn uninstall_skips_legacy_unmarked_unit_instead_of_deleting_it() {
	with_env(|root| {
		let dir = root.join("containers/systemd");
		std::fs::create_dir_all(&dir).unwrap();
		// A unit installed before ownership markers existed: same naming scheme
		// as a real 'app' unit, but no `# podup-owner:` marker anywhere in it, so
		// it cannot be proven to belong to 'app'.
		std::fs::write(
			dir.join("app-web.container"),
			"[Unit]\nDescription=web (podup)\n\n\
			 [Container]\nImage=nginx\nContainerName=app-web\n\n\
			 [Install]\nWantedBy=default.target\n",
		)
		.unwrap();
		let sc = FakeCtl::new();
		uninstall_quadlet(&sc, "app").unwrap();
		// Unproven ownership: skip it, never delete it.
		assert!(dir.join("app-web.container").is_file());
		// The reload still runs (uninstall is otherwise a no-op success, not an
		// error) even though nothing was removed.
		assert_eq!(sc.log().last().unwrap(), &vec!["daemon-reload".to_string()]);
	});
}

#[test]
fn rebuild_is_scoped_by_ownership_label_and_ignores_sibling_build_units() {
	with_env(|_root| {
		// 'app-extra' shares the `app-` filename prefix with 'app', so a naive
		// prefix match would treat its `.build`/container as buildable under
		// 'app' too.
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(BUILD).unwrap(),
			"app",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(BUILD).unwrap(),
			"app-extra",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		let sc = FakeCtl::new();
		rebuild_quadlet(&sc, "app", None).unwrap();
		assert_eq!(
			sc.log(),
			vec![
				vec!["restart".to_string(), "app-web-build.service".to_string()],
				vec!["restart".to_string(), "app-web.service".to_string()],
			]
		);
	});
}

#[test]
fn rebuild_restarts_build_then_container() {
	with_env(|_root| {
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(BUILD).unwrap(),
			"proj",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		let sc = FakeCtl::new();
		rebuild_quadlet(&sc, "proj", Some("web")).unwrap();
		assert_eq!(
			sc.log(),
			vec![
				vec!["restart".to_string(), "proj-web-build.service".to_string()],
				vec!["restart".to_string(), "proj-web.service".to_string()],
			]
		);
	});
}

#[test]
fn rebuild_unknown_service_errors_and_lists_valid_ones() {
	with_env(|_root| {
		install_quadlet(
			&FakeCtl::new(),
			&parse_str(BUILD).unwrap(),
			"proj",
			Path::new(BASE),
			true,
			false,
		)
		.unwrap();
		let err = rebuild_quadlet(&FakeCtl::new(), "proj", Some("nope")).unwrap_err();
		let msg = format!("{err}");
		assert!(msg.contains("has no build unit"), "{msg}");
		assert!(msg.contains("web"), "{msg}");
	});
}
