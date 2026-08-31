//! Pure rendering of the `podup-<project>.service` systemd user unit.
//!
//! The unit is a `Type=oneshot` `RemainAfterExit=yes` service that runs `podup
//! ... up -d` at boot and `podup ... stop` on stop. systemd has no cwd and no
//! relative-path context, so every path the unit embeds is absolute and every
//! argument is escaped per the systemd exec-line syntax.

use std::path::PathBuf;

/// Inputs to render a service-mode autostart unit. Every path must be absolute:
/// systemd resolves the `ExecStart`/`ExecStop` lines with no working directory of
/// its own, so a relative path would be interpreted against `/`.
///
/// `#[non_exhaustive]`: this gained a field once and will again as more of the
/// unit becomes configurable, so it is constructed with `..Default::default()`
/// rather than by a literal that breaks on every addition.
#[non_exhaustive]
#[derive(Default)]
pub struct ServiceUnitOpts {
	/// Absolute path to the `podup` executable.
	pub exe: PathBuf,
	/// Absolute compose-file paths, in `-f` order (later overrides earlier).
	pub compose_files: Vec<PathBuf>,
	/// Project name (already validated as a safe path component).
	pub project: String,
	/// Absolute working directory (the project base directory).
	pub working_dir: PathBuf,
	/// Active profiles, passed through as `--profile` flags.
	pub profiles: Vec<String>,
	/// Extra env files, passed through as `--env-file` flags.
	pub env_files: Vec<String>,
	/// Longest `stop_grace_period` across the project's services, in seconds.
	///
	/// systemd bounds the whole `ExecStop` independently of what podup does
	/// inside it, and its default is 90s — so a stack whose slowest container
	/// needs longer gets killed mid-stop at reboot, while a manual `podup stop`
	/// honours it. `None` leaves `TimeoutStopSec=` off, which is right when no
	/// service asks for anything unusual.
	pub max_stop_grace_secs: Option<u64>,
}

impl ServiceUnitOpts {
	/// The four values a unit cannot be rendered without. Everything else has a
	/// sensible empty default and is added with the `with_*` methods.
	pub fn new(
		exe: PathBuf,
		compose_files: Vec<PathBuf>,
		project: String,
		working_dir: PathBuf,
	) -> Self {
		Self {
			exe,
			compose_files,
			project,
			working_dir,
			profiles: Vec::new(),
			env_files: Vec::new(),
			max_stop_grace_secs: None,
		}
	}

	/// Active profiles, passed through as `--profile` flags. Builder-style.
	pub fn with_profiles(mut self, profiles: Vec<String>) -> Self {
		self.profiles = profiles;
		self
	}

	/// Extra env files, passed through as `--env-file` flags. Builder-style.
	pub fn with_env_files(mut self, env_files: Vec<String>) -> Self {
		self.env_files = env_files;
		self
	}

	/// The longest `stop_grace_period` in the project, so the unit can bound
	/// `ExecStop` above it rather than letting systemd's 90s default cut a
	/// slower stack off mid-shutdown. Builder-style.
	pub fn with_max_stop_grace_secs(mut self, secs: Option<u64>) -> Self {
		self.max_stop_grace_secs = secs;
		self
	}
}

/// Whether a token is safe to place on a systemd exec line without quoting:
/// only an unambiguous, shell-neutral subset of ASCII. Anything else (a space, a
/// quote, a control byte, a glob/redirect metacharacter) forces double-quoting.
fn is_bare_safe(token: &str) -> bool {
	!token.is_empty()
		&& token.bytes().all(|b| {
			b.is_ascii_alphanumeric()
				|| matches!(
					b,
					b'-' | b'_' | b'.' | b'/' | b':' | b'=' | b'@' | b'+' | b','
				)
		})
}

/// Quote a single argument for a systemd `ExecStart=`/`ExecStop=` line. Tokens
/// made only of the safe subset are emitted verbatim; everything else is wrapped
/// in double quotes with `\` and `"` (and the C-style control escapes systemd
/// understands) backslash-escaped, so a path with spaces survives as one argument.
///
/// A literal `%` is doubled to `%%` first, before the bare/quoted decision:
/// systemd expands `%`-specifiers (`%h`, `%i`, `%o`, ...) in a unit value
/// during specifier expansion, a pass that happens before the line is split
/// into arguments and runs whether or not the token ends up quoted. `%` is not
/// in `is_bare_safe`'s allowed set, so a token containing it already takes the
/// quoted path — but doubling it up front (rather than only inside the quoted
/// branch) means the escape holds even if that allowed set ever changes, and a
/// bare-looking token like `50%off` still comes out as `50%%off`.
fn quote_arg(token: &str) -> String {
	let token = token.replace('%', "%%");
	if is_bare_safe(&token) {
		return token;
	}
	let mut out = String::with_capacity(token.len() + 2);
	out.push('"');
	for ch in token.chars() {
		match ch {
			'"' => out.push_str("\\\""),
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\t' => out.push_str("\\t"),
			'\r' => out.push_str("\\r"),
			c => out.push(c),
		}
	}
	out.push('"');
	out
}

/// Reject any unit-embedded value containing ASCII control characters.
///
/// `WorkingDirectory=` (unlike exec-line tokens) takes the rest of its line
/// literally and honours no C-escapes, so a path with an embedded newline
/// would terminate the directive and inject arbitrary unit lines (e.g. an
/// `ExecStartPre=`). No legitimate path or flag value contains control bytes;
/// fail closed instead of trying to escape the unescapable.
pub fn validate_unit_opts(opts: &ServiceUnitOpts) -> Result<(), String> {
	fn check(field: &str, value: &str) -> Result<(), String> {
		if value.chars().any(|c| c.is_ascii_control()) {
			return Err(format!(
				"{field} contains a control character and cannot be embedded in a systemd unit: {value:?}"
			));
		}
		Ok(())
	}
	check("executable path", &opts.exe.to_string_lossy())?;
	check("working directory", &opts.working_dir.to_string_lossy())?;
	check("project name", &opts.project)?;
	for f in &opts.compose_files {
		check("compose file path", &f.to_string_lossy())?;
	}
	for p in &opts.profiles {
		check("profile", p)?;
	}
	for e in &opts.env_files {
		check("env file path", e)?;
	}
	Ok(())
}

/// The leading `podup` arguments shared by both the start and stop commands:
/// `-f <file>...  -p <project>  [--profile P]...  [--env-file E]...`. These must
/// precede the subcommand (`-f`/`-p` are not global flags).
fn leading_args(opts: &ServiceUnitOpts) -> Vec<String> {
	let mut args = Vec::new();
	for f in &opts.compose_files {
		args.push("-f".to_string());
		args.push(f.to_string_lossy().into_owned());
	}
	args.push("-p".to_string());
	args.push(opts.project.clone());
	for p in &opts.profiles {
		args.push("--profile".to_string());
		args.push(p.clone());
	}
	for e in &opts.env_files {
		args.push("--env-file".to_string());
		args.push(e.clone());
	}
	args
}

/// Render a full exec line: the absolute exe, the shared leading args, then the
/// command-specific trailing args, every token escaped and space-joined.
fn exec_line(opts: &ServiceUnitOpts, trailing: &[&str]) -> String {
	let mut tokens = Vec::new();
	tokens.push(opts.exe.to_string_lossy().into_owned());
	tokens.extend(leading_args(opts));
	tokens.extend(trailing.iter().map(|s| s.to_string()));
	tokens
		.iter()
		.map(|t| quote_arg(t))
		.collect::<Vec<_>>()
		.join(" ")
}

/// Render the full `.service` unit file content for service-mode autostart.
pub fn render_service_unit(opts: &ServiceUnitOpts) -> String {
	// `up -d`, not `up -d --build`: a boot must not depend on a build. A build
	// needs the network, takes minutes, and a registry that is briefly
	// unreachable would leave the stack down on an unattended machine. Build at
	// deploy time, where someone is watching.
	let start = exec_line(opts, &["up", "-d"]);
	// `stop`, not `down`: `down` REMOVES the containers, so a clean shutdown
	// would delete the stack and every boot would recreate it from scratch —
	// losing container identity and logs, and dragging the whole compose
	// front-end (.env, interpolation, the parse) onto the boot path. `stop`
	// leaves them on disk, which is exactly what ExecStart expects to find, and
	// honours each container's own stop_signal / stop_grace_period.
	let stop = exec_line(opts, &["stop"]);
	// Network ordering goes through Podman's user-scope shim, never through
	// `network-online.target` directly. That target is a system-manager concept
	// and stays inert in the `--user` instance, so naming it here would read as a
	// readiness gate and fire as nothing. Podman ships
	// `podman-user-wait-network-online.service` for exactly that gap
	// (containers/podman#22197): a `Type=oneshot` unit that polls
	// `systemctl is-active network-online.target` until the system target comes
	// up, which is the one thing a user unit can actually wait on.
	//
	// Quadlet mode gets this for free. `man podman-systemd.unit`, under *Implicit
	// network dependencies*, says the generator adds `Wants=`/`After=` on the
	// same shim to every `.container` unit it converts, so the files
	// `autostart/quadlet.rs` emits pick it up at boot. Service mode writes the
	// final unit itself and inherits nothing, and the reason applies here with
	// more force rather than less: Quadlet's `ExecStart` starts a container,
	// ours is `podup up -d`, which may pull an image, and rootless pasta builds
	// the container network at start time.
	//
	// Measured 2026-08-30: the shim first ships in Podman 5.3.0, and podup's
	// floor is 5.0. On 5.0 through 5.2 systemd finds no such unit, drops the
	// `Wants=`/`After=` with `LoadState=not-found`, and starts this unit clean
	// with `Result=success` and nothing in the journal, which is the behaviour
	// those versions already had.
	//
	// `WorkingDirectory=` takes the rest of its line literally — unlike an
	// exec-line token, it understands none of the C-style backslash escapes
	// `quote_arg` uses — but `%%` is not one of those escapes: it is systemd's
	// specifier-level escape, resolved during the same specifier-expansion pass
	// that runs over every unit-file value before the value is otherwise
	// interpreted. That pass does not care whether the value is a directive
	// meant to be split into words or taken whole, so doubling a literal `%`
	// here collapses back to one literal `%` exactly as it does on an exec
	// line, and reaches the filesystem unexpanded by any specifier.
	let workdir = opts.working_dir.display().to_string().replace('%', "%%");
	// `Description=` takes its value literally, exactly like `WorkingDirectory=`
	// above: systemd's specifier expansion runs over every unit-file value, not
	// only the ones this module treats specially. `opts.project` is gated
	// upstream by `is_safe_project_name` (which forbids `%`), but this module
	// must not lean on that external guarantee for its own %-invariant — every
	// other interpolated value here is escaped regardless of what validates it
	// elsewhere, so the project name is too.
	let project = opts.project.replace('%', "%%");
	// systemd bounds ExecStop on its own, at DefaultTimeoutStopUSec (90s), no
	// matter that `podup stop` honours each container's own grace period inside
	// it. Give it headroom over the slowest container rather than the exact
	// value: the stop has per-container teardown around it, and a bound equal to
	// the grace period would cut the last container off just as it finishes.
	// Quadlet mode already gets this right via StopTimeout=, so this closes an
	// inconsistency between the two modes rather than adding a new behaviour.
	let stop_timeout = match opts.max_stop_grace_secs {
		Some(secs) => format!("TimeoutStopSec={}\n", secs.saturating_add(30)),
		None => String::new(),
	};
	format!(
		"[Unit]\n\
		 Description=podup {project}\n\
		 Wants=podman-user-wait-network-online.service\n\
		 After=podman-user-wait-network-online.service\n\
		 \n\
		 [Service]\n\
		 Type=oneshot\n\
		 RemainAfterExit=yes\n\
		 WorkingDirectory={workdir}\n\
		 ExecStart={start}\n\
		 ExecStop={stop}\n\
		 {stop_timeout}\
		 \n\
		 [Install]\n\
		 WantedBy=default.target\n",
		project = project,
		workdir = workdir,
		start = start,
		stop = stop,
		stop_timeout = stop_timeout,
	)
}

#[cfg(test)]
mod tests {
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
		assert!(s.contains(
			"ExecStart=/usr/local/bin/podup -f /srv/app/docker-compose.yml -p app up -d"
		));
		assert!(
			s.contains("ExecStop=/usr/local/bin/podup -f /srv/app/docker-compose.yml -p app stop")
		);
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
		assert!(
			s.contains("-p app --profile prod --profile web --env-file /srv/app/.env.prod up -d")
		);
		assert!(
			s.contains("-p app --profile prod --profile web --env-file /srv/app/.env.prod stop")
		);
	}

	#[test]
	fn boot_neither_builds_nor_destroys() {
		// The contract, pinned. `--build` on ExecStart puts an image build on the
		// boot path of an unattended machine — it needs the network and a briefly
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
		assert!(s.contains(
			"ExecStart=\"/opt/my tools/podup\" -f \"/srv/my app/compose.yml\" -p app up -d"
		));
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
		// `50%off` has no space/quote/control byte — the only reason it must be
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
		assert!(s.contains(
			"ExecStart=/usr/local/bin/podup -f /srv/app/docker-compose.yml -p app up -d"
		));
	}

	#[test]
	fn render_service_unit_escapes_percent_in_project_description() {
		// `Description=` interpolates `opts.project` directly; a literal `%` in
		// it must be doubled exactly like every other in-unit value, so systemd's
		// specifier expansion does not treat e.g. `%h` as a specifier. This holds
		// regardless of the external `is_safe_project_name` gate — the module's
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
	/// reboot — the difference only appears during an unattended shutdown,
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
}
