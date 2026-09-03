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
pub(super) fn quote_arg_for_exec(token: &str) -> String {
	quote_arg(token)
}

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

/// The cadence the auto-update timer uses. `interval` is the `OnCalendar=`
/// word (`hourly`/`daily`/`weekly`) and the only one systemd accepts for the
/// three document presets, anything else has to be a longer expression like
/// `*-*-* 03:00:00`, and that surface is deliberately out of scope.
const ALLOWED_AUTO_UPDATE_INTERVALS: &[&str] = &["hourly", "daily", "weekly"];

/// Reject anything that is not one of the three word forms above. clap already
/// narrows this at the CLI level, so reaching here means a programmatic caller
/// (`InstallOptions::with_auto_update_interval`) passed something the user
/// never typed, surface it the same way any other bad interval would be
/// surfaced.
pub fn validate_auto_update_interval(interval: &str) -> Result<(), String> {
	if ALLOWED_AUTO_UPDATE_INTERVALS.contains(&interval) {
		Ok(())
	} else {
		Err(format!(
			"invalid --auto-update interval {interval:?} (expected one of: {})",
			ALLOWED_AUTO_UPDATE_INTERVALS.join(", ")
		))
	}
}

/// The `<unit-name>-update.service` filename.
pub fn update_service_file_name(project: &str) -> String {
	format!("podup-{project}-update.service")
}

/// The `<unit-name>-update.timer` filename.
pub fn update_timer_file_name(project: &str) -> String {
	format!("podup-{project}-update.timer")
}

/// Render the auto-update oneshot service: same leading arguments as the main
/// unit (so `-f`/`-p`/`--profile`/`--env-file` travel together), then
/// `up -d`. The timer fires it; systemd runs it `Type=oneshot` so each fire
/// is its own `podup up -d` invocation, not a long-running process.
pub fn render_update_service_unit(opts: &ServiceUnitOpts) -> String {
	let start = exec_line(opts, &["up", "-d"]);
	let workdir = opts.working_dir.display().to_string().replace('%', "%%");
	let project = opts.project.replace('%', "%%");
	format!(
		"[Unit]\n\
		 Description=podup {project} auto-update\n\
		 Wants=podman-user-wait-network-online.service\n\
		 After=podman-user-wait-network-online.service\n\
		 \n\
		 [Service]\n\
		 Type=oneshot\n\
		 WorkingDirectory={workdir}\n\
		 ExecStart={start}\n",
		project = project,
		workdir = workdir,
		start = start,
	)
}

/// Render the auto-update timer: `OnCalendar=<interval>`, `Persistent=true` so
/// missed fires (the host was off) catch up on next boot, and
/// `WantedBy=timers.target` so the standard timer enable path takes it.
pub fn render_update_timer_unit(project: &str, interval: &str) -> String {
	let project = project.replace('%', "%%");
	let interval_word = if let Err(e) = validate_auto_update_interval(interval) {
		// clap rejects unknown values before reaching here; this is the
		// programmatic-call guard. Fail loud rather than write a unit with a
		// bogus OnCalendar= value (systemd drops the timer silently in that
		// case and the user never knows).
		panic!("render_update_timer_unit called with bad interval: {e}");
	} else {
		interval
	};
	format!(
		"[Unit]\n\
		 Description=podup {project} auto-update timer\n\
		 \n\
		 [Timer]\n\
		 OnCalendar={interval_word}\n\
		 Persistent=true\n\
		 \n\
		 [Install]\n\
		 WantedBy=timers.target\n",
		project = project,
		interval_word = interval_word,
	)
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
#[path = "service_tests.rs"]
mod tests;
