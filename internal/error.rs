//! Error types for the podup library.
//!
//! All fallible operations return [`Result<T>`], which is an alias for
//! `std::result::Result<T, ComposeError>`.

use std::fmt;

/// All errors produced by podup.
///
/// `#[non_exhaustive]`: new variants may be added in a minor release, so
/// downstream `match` arms must include a wildcard.
#[derive(Debug)]
#[non_exhaustive]
pub enum ComposeError {
	/// The compose YAML could not be deserialized.
	Parse(serde_yaml::Error),
	/// A referenced compose/include/extends file does not exist.
	FileNotFound(String),
	/// An underlying filesystem operation failed.
	Io(std::io::Error),
	/// The Podman libpod API returned an error or could not be reached.
	Podman(crate::libpod::PodmanError),
	/// A named service is not defined in the compose file.
	ServiceNotFound(String),
	/// `depends_on` forms a cycle, so no valid start order exists.
	CircularDependency(String),
	/// A service has neither an `image:` nor a `build:` section.
	NoImageOrBuild(String),
	/// A `${VAR}` with the `?err` modifier was required but unset.
	RequiredVarNotSet {
		/// The variable name as it appeared in the compose file, without the
		/// `${}` or the `:?` suffix.
		var: String,
		/// The author's own explanation, taken verbatim from the text after
		/// `:?` in the interpolation. Empty when the form was bare `${VAR?}`.
		msg: String,
	},
	/// A `${…}` interpolation reference is malformed (e.g. an invalid character
	/// in the variable name, as in `${FOO BAR}` or `${FOO.BAR}`).
	InvalidSubstitution(String),
	/// A service did not become healthy within its dependency wait window.
	HealthCheckTimeout(String),
	/// A `ports:` entry could not be parsed.
	InvalidPort(String),
	/// A `kill` signal is empty, malformed, or not a recognised signal
	/// name/number. Forwarding it verbatim would let libpod silently default to
	/// SIGKILL, so it is rejected up front.
	InvalidSignal(String),
	/// Image build failed (context assembly or the Podman build step).
	Build(String),
	/// A `cp` (copy between a container and the host) operation failed — a missing
	/// destination directory, a non-directory path component, an unsupported
	/// endpoint, or a host-side packing/extraction error.
	Copy(String),
	/// `extends:` could not be resolved (missing file/service or a cycle).
	Extends(String),
	/// `include:` could not be resolved or merged.
	Include(String),
	/// The `watch` command failed (filesystem watch or sync action).
	Watch(String),
	/// A compose feature is recognized but unsupported on Podman/podup.
	Unsupported(String),
	/// A `run` container exited; carries its non-zero exit code so the CLI can
	/// propagate it as its own process exit status.
	RunExited(i64),
	/// An attached `up` was ended by SIGINT or SIGTERM rather than by its
	/// streams finishing. Carried as an error only so it reaches the exit-status
	/// mapping; it is not printed, because the operator who pressed Ctrl-C does
	/// not need to be told what they just did.
	Interrupted,
	/// A stream ended when it should not have: an attached `up` whose container
	/// was still running, or an unbounded `events` feed that returned at all
	/// (#1104). Carries a short description of which, since the detail (the
	/// container, the transport error) is already warned as it happens.
	StreamTruncated(String),
	/// `port` asked for a container port the service does not publish to
	/// the host. Carries the rendered sentence, `<service> publishes no host
	/// port for <port>/<proto>`; the CLI adds its own `error:` prefix. Its own
	/// variant so the message is not filed under a feature podup lacks
	/// (`Unsupported`) or a stream that ended early (`StreamTruncated`)
	/// (#1697).
	PortNotPublished(String),
	/// `podup update` (self-update) failed.
	Update(String),
	/// An `external: true` secret/config/network/volume is absent.
	ExternalNotFound(String),
	/// A service is scaled to more than one replica but publishes a fixed host
	/// port, which only one container can bind.
	ScalePortConflict {
		/// The compose key of the service, not a container name.
		service: String,
		/// The replica count that made the binding impossible; always above one.
		replicas: usize,
		/// The host ports the service pins, in the order compose declared them.
		/// Only fixed ports appear — a range or an unspecified host port cannot
		/// collide this way.
		ports: Vec<u16>,
	},
	/// A container being waited on (`up`/`start --wait`, or a `service_healthy`
	/// dependency) exited non-zero before becoming ready.
	WaitServiceExited {
		/// The container name Podman reported, replica suffix included, rather
		/// than the compose service key.
		container: String,
		/// The container's exit status. Non-zero by construction: a zero exit is
		/// not this error.
		code: i64,
	},
	/// A service requests more replicas than the configured ceiling, which would
	/// let an untrusted `deploy.replicas`/`scale:` drive unbounded container
	/// creation (host DoS).
	ReplicaLimitExceeded {
		/// The compose key of the service that asked for too many.
		service: String,
		/// The count requested, from `deploy.replicas` or `scale:`.
		replicas: usize,
		/// The ceiling in force when the request was refused.
		max: u32,
	},
	/// `start --wait --wait-timeout` elapsed before services became healthy.
	WaitTimeout {
		/// The budget that elapsed, in whole seconds, as `--wait-timeout`
		/// received it. Not the time actually spent waiting.
		secs: u64,
	},
	/// A replica index (`--index`) does not name a replica of the service (zero,
	/// or beyond the replica count). Kept distinct from [`Self::ServiceNotFound`]
	/// so the index hint renders outside the quoted service name.
	ReplicaIndex {
		/// The compose key of the service the index was meant to address.
		service: String,
		/// The index as given. Either zero, which is never valid because
		/// replicas are numbered from one, or past the service's replica count.
		index: u32,
	},
	/// A filesystem operation failed against a known path; carries the path so the
	/// message can name the offending file (Rust's `File::create`/`open` errors
	/// drop it).
	IoPath {
		/// The path the operation was attempted against, as resolved rather than
		/// as written in the compose file.
		path: String,
		/// The underlying failure. Carried separately because Rust's own
		/// `File::create`/`open` errors drop the path.
		source: std::io::Error,
	},
	/// A service's build context could not be accessed; names the service and the
	/// resolved context path instead of a bare `io error`.
	BuildContext {
		/// The compose key of the service whose build context could not be read.
		service: String,
		/// The resolved context directory, anchored against the project
		/// directory rather than the current one.
		path: String,
		/// The underlying failure, kept so the message can name both the service
		/// and the file rather than reporting a bare io error.
		source: std::io::Error,
	},
	/// A targeted service container is not running (e.g. `exec`/`attach` against a
	/// stopped or never-created container).
	NotRunning(String),
	/// An `exec` session could not be launched. Most often the requested
	/// `--user`/`--workdir` does not resolve inside the container and the libpod
	/// exec-start stalls without returning a response head; podup bounds that wait
	/// with an exec-specific deadline and surfaces this instead of pinning the CLI
	/// for the full read timeout and then reporting a misleading socket-timeout.
	/// The string is a ready-to-print message.
	ExecFailed(String),
	/// The `-t/--timeout` shutdown grace was given an unusable value (a number
	/// below `-1`). `-1` means "wait indefinitely" (docker parity) and any
	/// non-negative value is a second count; everything else is rejected here
	/// rather than forwarded to libpod as a raw `HTTP 400`.
	InvalidTimeout(i32),
	/// An explicitly requested env file (`--env-file` or a service `env_file:`)
	/// could not be read or parsed — a missing/unreadable path or a malformed
	/// entry such as an unterminated quoted value. The string is a ready-to-print
	/// message.
	EnvFile(String),
	/// A `podup autostart` operation failed — a `systemctl --user`/`loginctl`
	/// command could not run or returned non-zero, a unit file could not be
	/// written/removed, or the requested mode is not yet available. The string is
	/// a ready-to-print message.
	Autostart(String),
	/// A `service_healthy` dependency did not become ready. Wraps the shared
	/// readiness error in an `Arc` so one poller's result can fan out to every
	/// dependent waiting on the same container (the error type is otherwise not
	/// `Clone`). Transparent: it displays as, and sources, the wrapped error.
	DependencyNotReady(std::sync::Arc<ComposeError>),
}

impl ComposeError {
	/// Peel [`Self::DependencyNotReady`] wrappers to the underlying cause.
	///
	/// The readiness fan-out wraps a poller's error so it can be shared; callers
	/// that classify an error by variant (e.g. the CLI's exit-code mapping) want
	/// the real cause, not the wrapper.
	pub fn innermost(&self) -> &ComposeError {
		let mut e = self;
		while let Self::DependencyNotReady(inner) = e {
			e = inner;
		}
		e
	}
}

/// Escape control characters (tabs, newlines, ESC, …) in an interpolated,
/// possibly-untrusted name before it reaches a terminal, so a crafted
/// service/container name cannot emit raw escape sequences. Printable characters
/// (including non-ASCII) pass through unchanged; only borrows when nothing needs
/// escaping.
fn sanitize_name(s: &str) -> std::borrow::Cow<'_, str> {
	if s.chars().any(char::is_control) {
		s.chars()
			.flat_map(|c| {
				if c.is_control() {
					c.escape_default().collect::<Vec<_>>()
				} else {
					vec![c]
				}
			})
			.collect::<String>()
			.into()
	} else {
		std::borrow::Cow::Borrowed(s)
	}
}

impl fmt::Display for ComposeError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			// Report only the parser's location, never the raw `serde_yaml`
			// message: that message embeds the offending scalar verbatim (the file's
			// own content), which would echo a non-compose file pointed at with `-f`
			// straight onto stderr. Location (line/column) is enough to find the
			// problem without leaking the bytes.
			Self::Parse(e) => match e.location() {
				Some(loc) => write!(
					f,
					"failed to parse compose file at line {}, column {}",
					loc.line(),
					loc.column()
				),
				None => write!(f, "failed to parse compose file"),
			},
			Self::FileNotFound(s) => write!(f, "compose file not found: {}", sanitize_name(s)),
			Self::Io(e) => write!(f, "io error: {e}"),
			Self::Podman(e) => write!(f, "podman error: {e}"),
			Self::ServiceNotFound(s) => write!(f, "service '{}' not found", sanitize_name(s)),
			Self::CircularDependency(s) => write!(f, "{s}"),
			Self::NoImageOrBuild(s) => {
				write!(
					f,
					"service '{}' has no image or build config",
					sanitize_name(s)
				)
			}
			Self::RequiredVarNotSet { var, msg } => {
				write!(f, "required variable '{var}' is not set: {msg}")
			}
			Self::InvalidSubstitution(s) => {
				write!(f, "invalid variable substitution: {s}")
			}
			Self::HealthCheckTimeout(s) => {
				write!(
					f,
					"health check timeout for container '{}'",
					sanitize_name(s)
				)
			}
			Self::InvalidPort(s) => write!(f, "invalid port mapping: {s}"),
			Self::InvalidSignal(s) => write!(f, "invalid signal: {s}"),
			Self::Build(s) => write!(f, "build error: {s}"),
			Self::Copy(s) => write!(f, "cp error: {s}"),
			Self::Extends(s) => write!(f, "extends error: {s}"),
			Self::Include(s) => write!(f, "include error: {s}"),
			Self::Watch(s) => write!(f, "watch error: {s}"),
			Self::Unsupported(s) => write!(f, "unsupported feature: {s}"),
			Self::RunExited(code) => write!(f, "run container exited with code {code}"),
			Self::Interrupted => write!(f, "interrupted"),
			Self::StreamTruncated(what) => write!(f, "{what}"),
			Self::PortNotPublished(what) => write!(f, "{what}"),
			Self::Update(s) => write!(f, "update error: {s}"),
			Self::ExternalNotFound(s) => write!(f, "external resource not found: {s}"),
			Self::ScalePortConflict {
				service,
				replicas,
				ports,
			} => {
				let ports = ports
					.iter()
					.map(u16::to_string)
					.collect::<Vec<_>>()
					.join(", ");
				write!(
					f,
					"service '{service}' publishes fixed host port(s) [{ports}] but is scaled to \
					 {replicas} replicas; only one container can bind a host port. Use one of:\n  \
					 - remove the host port (e.g. `- \"80\"`) so Podman assigns a random one per \
					 replica\n  - put the service behind a reverse proxy and publish only the \
					 proxy's port\n  - reduce the service to a single replica"
				)
			}
			Self::WaitServiceExited { container, code } => write!(
				f,
				"container '{}' exited with code {code} while waiting for it to be ready",
				sanitize_name(container)
			),
			Self::ReplicaLimitExceeded {
				service,
				replicas,
				max,
			} => write!(
				f,
				"service '{service}' requests {replicas} replicas, which exceeds the limit of \
				 {max}; lower the count or raise the limit with PODUP_MAX_REPLICAS"
			),
			Self::WaitTimeout { secs } => write!(
				f,
				"timed out after {secs}s waiting for services to become healthy"
			),
			Self::ReplicaIndex { service, index } => write!(
				f,
				"service '{}' has no replica {index} (replica indexes are 1-based)",
				sanitize_name(service)
			),
			Self::IoPath { path, source } => {
				write!(f, "io error: {}: {source}", sanitize_name(path))
			}
			Self::BuildContext {
				service,
				path,
				source,
			} => write!(
				f,
				"build context '{}' for service '{}': {source}",
				sanitize_name(path),
				sanitize_name(service)
			),
			Self::NotRunning(s) => write!(f, "service '{}' is not running", sanitize_name(s)),
			Self::ExecFailed(s) => write!(f, "exec failed: {s}"),
			Self::InvalidTimeout(secs) => write!(
				f,
				"invalid --timeout {secs}: use -1 to wait indefinitely or a non-negative number of seconds"
			),
			Self::EnvFile(s) => write!(f, "{s}"),
			Self::Autostart(s) => write!(f, "{s}"),
			// Transparent: the wrapper only exists to make the cause shareable.
			Self::DependencyNotReady(inner) => write!(f, "{inner}"),
		}
	}
}

impl std::error::Error for ComposeError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Parse(e) => Some(e),
			Self::Io(e) => Some(e),
			Self::Podman(e) => Some(e),
			Self::IoPath { source, .. } | Self::BuildContext { source, .. } => Some(source),
			Self::DependencyNotReady(inner) => Some(inner),
			_ => None,
		}
	}
}

impl From<serde_yaml::Error> for ComposeError {
	fn from(e: serde_yaml::Error) -> Self {
		Self::Parse(e)
	}
}

impl From<std::io::Error> for ComposeError {
	fn from(e: std::io::Error) -> Self {
		Self::Io(e)
	}
}

impl From<crate::libpod::PodmanError> for ComposeError {
	fn from(e: crate::libpod::PodmanError) -> Self {
		Self::Podman(e)
	}
}

/// Convenience alias for `std::result::Result<T, ComposeError>`.
pub type Result<T> = std::result::Result<T, ComposeError>;

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
