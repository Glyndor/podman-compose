//! Pure rendering and validation for start mode: a unit whose `ExecStart` is
//! `podman start`, so the boot path resumes the container Podman already holds
//! rather than reconciling it against the compose file.
//!
//! The other two modes make the world match the file at boot. Service mode
//! parses the compose file, interpolates the environment and may build; quadlet
//! mode hands systemd a description Podman reconciles. Both are the right shape
//! for a deploy. Podman is daemonless and its store survives a reboot, so every
//! setting is already baked into the container definition when it is created,
//! and `podman start` restores the lot with no compose file, no `.env`, no
//! registry and no build on the path.
//!
//! The failure semantics are the point rather than a side effect. A container
//! missing at boot means a deploy went wrong, and booting cannot fix a broken
//! deploy: failing loudly in the journal is correct there, and rebuilding it
//! silently is not. Deploy reconciles, boot restores.

use std::path::PathBuf;

use crate::compose::types::{ComposeFile, Service};

/// Inputs to render a start-mode autostart unit. Both paths must be absolute:
/// systemd resolves an exec line with no working directory of its own.
#[non_exhaustive]
#[derive(Default)]
pub struct StartUnitOpts {
	/// Absolute path to the `podman` executable.
	pub podman: PathBuf,
	/// Project name (already validated as a safe path component).
	pub project: String,
	/// The one container this project resolves to.
	pub container: String,
	/// The service's `stop_grace_period`, in seconds, when it set one.
	pub stop_grace_secs: Option<u64>,
}

impl StartUnitOpts {
	/// The three values a unit cannot be rendered without.
	pub fn new(podman: PathBuf, project: String, container: String) -> Self {
		Self {
			podman,
			project,
			container,
			stop_grace_secs: None,
		}
	}

	/// The service's `stop_grace_period`, so the unit can bound `ExecStop` above
	/// it rather than letting systemd's 90s default cut a slower container off
	/// mid-shutdown. Builder-style.
	pub fn with_stop_grace_secs(mut self, secs: Option<u64>) -> Self {
		self.stop_grace_secs = secs;
		self
	}
}

/// Reject any unit-embedded value carrying ASCII control characters, for the
/// same reason `service::validate_unit_opts` does: a value with an
/// embedded newline would terminate its directive and inject arbitrary unit
/// lines. No legitimate path or container name contains control bytes.
pub fn validate_start_unit_opts(opts: &StartUnitOpts) -> Result<(), String> {
	fn check(field: &str, value: &str) -> Result<(), String> {
		if value.chars().any(|c| c.is_ascii_control()) {
			return Err(format!(
				"{field} contains a control character and cannot be embedded in a systemd unit: {value:?}"
			));
		}
		Ok(())
	}
	check("podman path", &opts.podman.to_string_lossy())?;
	check("project name", &opts.project)?;
	check("container name", &opts.container)
}

/// Why a project cannot use start mode. Carried as a distinct type so the
/// caller renders one message and the tests assert the reason rather than the
/// wording.
#[derive(Debug, PartialEq, Eq)]
pub enum StartModeRefusal {
	/// The compose file defines no services at all.
	NoServices,
	/// More than one service. `podman start` waits for nothing, so a project
	/// with `depends_on` (and especially `condition: service_healthy`) needs
	/// ordering between units, which is what quadlet mode already derives.
	MultipleServices(Vec<String>),
	/// One service, but scaled past a single replica.
	MultipleReplicas {
		/// The one service the file defines.
		service: String,
		/// How many replicas it declares.
		replicas: usize,
	},
}

impl std::fmt::Display for StartModeRefusal {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::NoServices => write!(
				f,
				"start mode needs exactly one service and this project defines none"
			),
			Self::MultipleServices(names) => write!(
				f,
				"start mode supports single-service projects only, and this one defines {}: {}.\n\
				 `podman start` waits for nothing, so a project with `depends_on` needs ordering \
				 between units. Use `--mode quadlet`, which derives that ordering from the \
				 compose file, or `--mode service`.",
				names.len(),
				names.join(", ")
			),
			Self::MultipleReplicas { service, replicas } => write!(
				f,
				"start mode supports a single container and service '{service}' resolves to \
				 {replicas} replicas. Use `--mode quadlet` or `--mode service`."
			),
		}
	}
}

/// The one container a project resolves to, or why it does not resolve to one.
///
/// The name must agree with what the engine created, so it comes from
/// `engine::sole_replica_name` rather than being spelled out again
/// here: a second copy of the naming rule would drift from the first one
/// silently, and the unit would name a container that does not exist.
pub fn sole_container(file: &ComposeFile, project: &str) -> Result<String, StartModeRefusal> {
	let mut names: Vec<&String> = file.services.keys().collect();
	names.sort();
	let (name, service): (&String, &Service) = match names.as_slice() {
		[] => return Err(StartModeRefusal::NoServices),
		[only] => (*only, &file.services[*only]),
		many => {
			return Err(StartModeRefusal::MultipleServices(
				many.iter().map(|s| (*s).clone()).collect(),
			))
		}
	};
	let replicas = crate::engine::declared_replicas(service);
	if replicas != 1 {
		return Err(StartModeRefusal::MultipleReplicas {
			service: name.clone(),
			replicas,
		});
	}
	Ok(crate::engine::sole_replica_name(project, name, service))
}

/// Render the full `.service` unit for start mode.
pub fn render_start_unit(opts: &StartUnitOpts) -> String {
	// Both exec lines take the container name, never the compose file: that is
	// the whole mode. `podman start` restores the container definition from
	// Podman's store, which survives a reboot, so nothing here needs the file,
	// the environment, a registry or a build.
	//
	// `stop`, not `rm`: the container must still be there for the next boot's
	// ExecStart to find, exactly as service mode uses `stop` rather than `down`.
	//
	// Network ordering matches service mode and is there for the same reason
	// (#1616), though it costs less here: `podman start` pulls nothing, so the
	// wait guards the container's own networking rather than an image fetch.
	//
	// A literal `%` is doubled in every interpolated value. systemd's specifier
	// expansion runs over unit-file values before anything else interprets them,
	// so an undoubled `%h`/`%o` in a container name or path would expand.
	let podman = quote_arg(&opts.podman.to_string_lossy());
	let container = quote_arg(&opts.container);
	let project = opts.project.replace('%', "%%");
	// systemd bounds ExecStop at DefaultTimeoutStopUSec (90s) regardless of what
	// `podman stop` honours inside it. Give it headroom over the container's own
	// grace period rather than the exact value, matching service mode.
	let stop_timeout = match opts.stop_grace_secs {
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
		 ExecStart={podman} start {container}\n\
		 ExecStop={podman} stop {container}\n\
		 {stop_timeout}\
		 \n\
		 [Install]\n\
		 WantedBy=default.target\n",
	)
}

/// Quote one exec-line token, doubling a literal `%` first. Same rules as
/// [`super::service`]; kept as its own copy of the call rather than sharing the
/// private helper, since only these two tokens need it here.
fn quote_arg(token: &str) -> String {
	super::service::quote_arg_for_exec(token)
}
