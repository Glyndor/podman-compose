use super::*;
use std::path::PathBuf;

fn opts_single() -> ServiceUnitOpts {
	ServiceUnitOpts {
		exe: PathBuf::from("/usr/local/bin/podup"),
		compose_files: vec![PathBuf::from("/srv/app/docker-compose.yml")],
		project: "app".to_string(),
		working_dir: PathBuf::from("/srv/app"),
		profiles: Vec::new(),
		env_files: Vec::new(),
		max_stop_grace_secs: None,
	}
}

#[test]
fn renders_single_file_unit() {
	let s = render_service_unit(&opts_single());
	assert!(s.contains("Description=podup app"));
	assert!(s.contains("Type=oneshot"));
	assert!(s.contains("RemainAfterExit=yes"));
	assert!(s.contains("WorkingDirectory=/srv/app"));
	assert!(s.contains("WantedBy=default.target"));
	assert!(
		s.contains("ExecStart=/usr/local/bin/podup -f /srv/app/docker-compose.yml -p app up -d")
	);
	assert!(s.contains("ExecStop=/usr/local/bin/podup -f /srv/app/docker-compose.yml -p app stop"));
}

#[test]
fn renders_multiple_files_in_order() {
	let mut o = opts_single();
	o.compose_files = vec![
		PathBuf::from("/srv/app/base.yml"),
		PathBuf::from("/srv/app/override.yml"),
	];
	let s = render_service_unit(&o);
	assert!(s.contains(
		"ExecStart=/usr/local/bin/podup -f /srv/app/base.yml -f /srv/app/override.yml -p app up -d"
	));
	assert!(s.contains(
		"ExecStop=/usr/local/bin/podup -f /srv/app/base.yml -f /srv/app/override.yml -p app stop"
	));
}

#[test]
fn includes_profiles_and_env_files() {
	let mut o = opts_single();
	o.profiles = vec!["prod".to_string(), "web".to_string()];
	o.env_files = vec!["/srv/app/.env.prod".to_string()];
	let s = render_service_unit(&o);
	assert!(s.contains("-p app --profile prod --profile web --env-file /srv/app/.env.prod up -d"));
	assert!(s.contains("-p app --profile prod --profile web --env-file /srv/app/.env.prod stop"));
}

#[test]
fn boot_neither_builds_nor_destroys() {
	// The contract, pinned. `--build` on ExecStart puts an image build on the
	// boot path of an unattended machine: it needs the network and a briefly
	// unreachable registry leaves the stack down. `down` on ExecStop removes
	// the containers, so a clean shutdown would delete the stack and every
	// boot would recreate it. Both shipped in 1.9.0; neither may come back
	// without this test being deleted on purpose.
	let s = render_service_unit(&opts_single());
	assert!(
		!s.contains("--build"),
		"a boot must not depend on a build:\n{s}"
	);
	assert!(
		!s.contains(" down"),
		"ExecStop must stop, not remove the containers:\n{s}"
	);
}

/// #1616: service mode writes the final unit, so it inherits none of the
/// ordering Quadlet's generator adds on its own. What has to hold is that the
/// unit waits for the network, and that it waits through Podman's user-scope
/// shim rather than through `network-online.target`, which is inert in the
/// `--user` instance.
///
/// The assertion this replaced only forbade the string `network-online.target`,
/// which a unit carrying no ordering at all also satisfies. That is the state
/// this test exists to rule out, so it asserts the keys that must be present
/// first and the spelling that must not appear second.
#[test]
fn orders_against_the_user_scope_network_shim() {
	let s = render_service_unit(&opts_single());
	assert!(
		s.contains("Wants=podman-user-wait-network-online.service"),
		"the unit must pull in the network shim:\n{s}"
	);
	assert!(
		s.contains("After=podman-user-wait-network-online.service"),
		"wanting the shim without ordering after it starts both at once, \
		 which is the same as not waiting:\n{s}"
	);
	for key in ["Wants=", "After=", "Requires=", "BindsTo=", "PartOf="] {
		assert!(
			!s.contains(&format!("{key}network-online.target")),
			"a `--user` unit must not depend on the system target directly, \
			 since it never activates there:\n{s}"
		);
	}
}

#[test]
fn quotes_paths_with_spaces() {
	let mut o = opts_single();
	o.exe = PathBuf::from("/opt/my tools/podup");
	o.compose_files = vec![PathBuf::from("/srv/my app/compose.yml")];
	o.working_dir = PathBuf::from("/srv/my app");
	let s = render_service_unit(&o);
	// The exe and the compose path are double-quoted as single arguments.
	assert!(
		s.contains("ExecStart=\"/opt/my tools/podup\" -f \"/srv/my app/compose.yml\" -p app up -d")
	);
	// WorkingDirectory takes the rest of the line literally, so it is not quoted.
	assert!(s.contains("WorkingDirectory=/srv/my app"));
}

#[test]
fn ends_with_newline() {
	assert!(render_service_unit(&opts_single()).ends_with("WantedBy=default.target\n"));
}

#[test]
fn validate_rejects_control_chars_in_workdir() {
	let mut o = opts_single();
	o.working_dir = PathBuf::from("/srv/app\nExecStartPre=/bin/evil");
	let err = validate_unit_opts(&o).unwrap_err();
	assert!(err.contains("working directory"), "{err}");
}

#[test]
fn validate_rejects_control_chars_in_exe_and_files() {
	let mut o = opts_single();
	o.exe = PathBuf::from("/usr/local/bin/pod\x07up");
	assert!(validate_unit_opts(&o).is_err());

	let mut o = opts_single();
	o.compose_files = vec![PathBuf::from("/srv/app/com\npose.yml")];
	assert!(validate_unit_opts(&o).is_err());

	let mut o = opts_single();
	o.env_files = vec!["/srv/app/.env\r".to_string()];
	assert!(validate_unit_opts(&o).is_err());
}

#[test]
fn validate_accepts_normal_paths() {
	assert!(validate_unit_opts(&opts_single()).is_ok());
	let mut o = opts_single();
	o.working_dir = PathBuf::from("/srv/my app (prod)");
	assert!(validate_unit_opts(&o).is_ok());
}

#[test]
fn bare_safe_accepts_paths_rejects_spaces() {
	assert!(is_bare_safe("/srv/app/compose.yml"));
	assert!(is_bare_safe("app-1_v2.0"));
	assert!(!is_bare_safe("has space"));
	assert!(!is_bare_safe(""));
	assert!(!is_bare_safe("a\"b"));
}

#[test]
fn quote_arg_escapes_quotes_and_backslashes() {
	assert_eq!(quote_arg("a b"), "\"a b\"");
	assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
	assert_eq!(quote_arg("a\\b"), "\"a\\\\b\"");
}

// --- bug: `%`-specifiers are not escaped in unit values (systemd expands
// `%h`/`%i`/`%o`/... in every unit-file value, exec tokens and
// `WorkingDirectory=` alike; a literal `%` must be doubled to `%%` or a path
// like `/srv/50%off` gets `%o`-expanded, mis-resolving or failing to start). ---

#[test]
fn quote_arg_doubles_percent_even_in_a_bare_looking_token() {
	// `50%off` has no space/quote/control byte; the only reason it must be
	// quoted at all is the `%`, and the `%` itself must be doubled so systemd's
	// specifier expansion collapses it back to one literal `%` instead of
	// trying to expand `%o` as a specifier.
	assert_eq!(quote_arg("50%off"), "\"50%%off\"");
	assert_eq!(quote_arg("100%"), "\"100%%\"");
}

#[test]
fn render_service_unit_escapes_percent_in_working_directory() {
	let mut o = opts_single();
	o.working_dir = PathBuf::from("/srv/50%off");
	let s = render_service_unit(&o);
	assert!(
		s.contains("WorkingDirectory=/srv/50%%off"),
		"WorkingDirectory must double a literal '%' so systemd does not expand \
		 '%o' as a specifier:\n{s}"
	);
	assert!(!s.contains("WorkingDirectory=/srv/50%off\n"), "{s}");
}

#[test]
fn render_service_unit_escapes_percent_in_exec_line_tokens() {
	let mut o = opts_single();
	o.compose_files = vec![PathBuf::from("/srv/50%off/docker-compose.yml")];
	let s = render_service_unit(&o);
	assert!(
		s.contains("50%%off/docker-compose.yml"),
		"an exec-line token containing '%' must render it doubled as '%%':\n{s}"
	);
	assert!(!s.contains("50%off/docker-compose.yml"), "{s}");
}

#[test]
fn render_service_unit_normal_path_round_trips_unchanged() {
	// A path with no '%' must not be touched by the escaping fix.
	let s = render_service_unit(&opts_single());
	assert!(s.contains("WorkingDirectory=/srv/app"));
	assert!(
		s.contains("ExecStart=/usr/local/bin/podup -f /srv/app/docker-compose.yml -p app up -d")
	);
}

#[test]
fn render_service_unit_escapes_percent_in_project_description() {
	// `Description=` interpolates `opts.project` directly; a literal `%` in
	// it must be doubled exactly like every other in-unit value, so systemd's
	// specifier expansion does not treat e.g. `%h` as a specifier. This holds
	// regardless of the external `is_safe_project_name` gate: the module's
	// own %-invariant should not depend on it.
	let mut o = opts_single();
	o.project = "50%h".to_string();
	let s = render_service_unit(&o);
	assert!(
		s.contains("Description=podup 50%%h"),
		"Description= must double a literal '%' in the project name:\n{s}"
	);
	assert!(!s.contains("Description=podup 50%h\n"), "{s}");
}

#[test]
fn validate_accepts_percent_in_paths() {
	// A literal '%' is a legitimate path/flag character (e.g. `/srv/50%off`);
	// it must be escaped at render time, never rejected at validation time.
	let mut o = opts_single();
	o.working_dir = PathBuf::from("/srv/50%off");
	o.compose_files = vec![PathBuf::from("/srv/50%off/docker-compose.yml")];
	assert!(validate_unit_opts(&o).is_ok());
}

/// #1093: systemd bounds `ExecStop` independently of what podup does inside
/// it, at a 90s default. A stack whose slowest container needs longer stops
/// cleanly when a human runs `podup stop` and gets killed mid-stop at
/// reboot; the difference only appears during an unattended shutdown,
/// which is the worst version of it.
#[test]
fn render_service_unit_bounds_stop_above_the_longest_grace_period() {
	let mut o = opts_single();
	o.max_stop_grace_secs = Some(120);
	let s = render_service_unit(&o);
	assert!(
		s.contains("TimeoutStopSec=150"),
		"expected the longest grace plus headroom:\n{s}"
	);
}

/// Headroom, not the exact value: the stop has per-container teardown around
/// it, so a bound equal to the grace period would cut the last container off
/// just as it finishes.
#[test]
fn stop_timeout_leaves_headroom_over_the_grace_period() {
	let mut o = opts_single();
	o.max_stop_grace_secs = Some(10);
	let s = render_service_unit(&o);
	let line = s
		.lines()
		.find(|l| l.starts_with("TimeoutStopSec="))
		.expect("the key is present");
	let secs: u64 = line.trim_start_matches("TimeoutStopSec=").parse().unwrap();
	assert!(secs > 10, "must exceed the grace period, got {secs}");
}

/// No service asking for anything unusual leaves the key off entirely, so
/// systemd keeps its own default rather than podup restating it.
#[test]
fn no_grace_period_emits_no_stop_timeout() {
	let s = render_service_unit(&opts_single());
	assert!(!s.contains("TimeoutStopSec"), "{s}");
}

// ---------------------------------------------------------------------------
// --auto-update: the sibling `<unit>-update.service` oneshot and `<unit>
// -update.timer` schedule (render-only; install/uninstall are exercised in
// `tests.rs`).
// ---------------------------------------------------------------------------

fn opts_single_for_timer() -> super::ServiceUnitOpts {
	super::ServiceUnitOpts {
		exe: std::path::PathBuf::from("/usr/local/bin/podup"),
		compose_files: vec![std::path::PathBuf::from("/srv/app/docker-compose.yml")],
		project: "app".to_string(),
		working_dir: std::path::PathBuf::from("/srv/app"),
		profiles: Vec::new(),
		env_files: Vec::new(),
		max_stop_grace_secs: None,
	}
}

/// The auto-update oneshot unit carries the same leading arguments as the
/// main unit (so `-f`/`-p`/`--profile`/`--env-file` travel together) and ends
/// in `up -d`. Without the matching trailing arguments, an `--auto-update`
/// timer firing `podup up` against the wrong project would be the worst
/// possible kind of quiet failure.
#[test]
fn autostart_update_service_uses_same_leading_args_then_up_minus_d() {
	let opts = opts_single_for_timer();
	let s = super::render_update_service_unit(&opts);
	assert!(s.contains("Type=oneshot"), "{s}");
	assert!(
		s.contains("ExecStart=/usr/local/bin/podup -f /srv/app/docker-compose.yml -p app up -d"),
		"{s}"
	);
	assert!(s.contains("WorkingDirectory=/srv/app"), "{s}");
	// The timer is what gets enabled; a oneshot with its own [Install] could be
	// enabled on its own and fire at every login instead of on the schedule.
	assert!(!s.contains("[Install]"), "{s}");
}

/// The timer carries `OnCalendar=<word>`, `Persistent=true` (missed fires
/// catch up on next boot), and `WantedBy=timers.target`.
#[test]
fn autostart_update_timer_carries_on_calendar_persistent_and_timers_target() {
	for word in ["hourly", "daily", "weekly"] {
		let s = super::render_update_timer_unit("app", word);
		assert!(
			s.contains(&format!("OnCalendar={word}")),
			"word {word}: {s}"
		);
		assert!(s.contains("Persistent=true"), "{s}");
		assert!(s.contains("WantedBy=timers.target"), "{s}");
	}
}

/// An unknown interval is rejected, the same way the CLI does, but the
/// programmatic helper does it in `validate_auto_update_interval`, since the
/// unit-renderer cannot return an error and silently emitting a bogus
/// `OnCalendar=` would leave the user without a working timer.
#[test]
fn autostart_auto_update_rejects_an_unknown_interval() {
	let err = super::validate_auto_update_interval("biweekly")
		.expect_err("biweekly must not be accepted");
	assert!(
		err.contains("biweekly") && err.contains("hourly") && err.contains("weekly"),
		"the error must name the offending value and the three allowed spellings: {err}"
	);
}
