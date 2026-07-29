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
	RequiredVarNotSet { var: String, msg: String },
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
	/// An attached `up` lost a log stream while its container was still running,
	/// so the output the user was watching is truncated rather than finished
	/// (#1104). Which container is named in a warning as it happens; this only
	/// carries the failure to the exit status.
	StreamTruncated,
	/// `podup update` (self-update) failed.
	Update(String),
	/// An `external: true` secret/config/network/volume is absent.
	ExternalNotFound(String),
	/// A service is scaled to more than one replica but publishes a fixed host
	/// port, which only one container can bind.
	ScalePortConflict {
		service: String,
		replicas: usize,
		ports: Vec<u16>,
	},
	/// A container being waited on (`up`/`start --wait`, or a `service_healthy`
	/// dependency) exited non-zero before becoming ready.
	WaitServiceExited { container: String, code: i64 },
	/// A service requests more replicas than the configured ceiling, which would
	/// let an untrusted `deploy.replicas`/`scale:` drive unbounded container
	/// creation (host DoS).
	ReplicaLimitExceeded {
		service: String,
		replicas: usize,
		max: u32,
	},
	/// `start --wait --wait-timeout` elapsed before services became healthy.
	WaitTimeout { secs: u64 },
	/// A replica index (`--index`) does not name a replica of the service (zero,
	/// or beyond the replica count). Kept distinct from [`Self::ServiceNotFound`]
	/// so the index hint renders outside the quoted service name.
	ReplicaIndex { service: String, index: u32 },
	/// A filesystem operation failed against a known path; carries the path so the
	/// message can name the offending file (Rust's `File::create`/`open` errors
	/// drop it).
	IoPath {
		path: String,
		source: std::io::Error,
	},
	/// A service's build context could not be accessed; names the service and the
	/// resolved context path instead of a bare `io error`.
	BuildContext {
		service: String,
		path: String,
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
			Self::StreamTruncated => write!(
				f,
				"log stream ended while the container was still running: output is incomplete"
			),
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
mod tests {
	use super::ComposeError;

	#[test]
	fn display_covers_all_variants() {
		let cases: &[(&str, ComposeError)] = &[
			(
				"failed to parse compose file",
				ComposeError::Parse(serde_yaml::from_str::<serde_yaml::Value>(":\0").unwrap_err()),
			),
			(
				"compose file not found: f",
				ComposeError::FileNotFound("f".into()),
			),
			("io error:", ComposeError::Io(std::io::Error::other("x"))),
			(
				"service 's' not found",
				ComposeError::ServiceNotFound("s".into()),
			),
			("c", ComposeError::CircularDependency("c".into())),
			(
				"service 'svc' has no image or build config",
				ComposeError::NoImageOrBuild("svc".into()),
			),
			(
				"required variable 'V' is not set: reason",
				ComposeError::RequiredVarNotSet {
					var: "V".into(),
					msg: "reason".into(),
				},
			),
			(
				"health check timeout for container 'c'",
				ComposeError::HealthCheckTimeout("c".into()),
			),
			(
				"invalid port mapping: p",
				ComposeError::InvalidPort("p".into()),
			),
			(
				"podman error:",
				ComposeError::Podman(crate::libpod::PodmanError::Api {
					status: 500,
					message: "boom".into(),
				}),
			),
			(
				"invalid variable substitution: bad",
				ComposeError::InvalidSubstitution("bad".into()),
			),
			("build error: b", ComposeError::Build("b".into())),
			("cp error: c", ComposeError::Copy("c".into())),
			("extends error: e", ComposeError::Extends("e".into())),
			("include error: i", ComposeError::Include("i".into())),
			("watch error: w", ComposeError::Watch("w".into())),
			(
				"unsupported feature: u",
				ComposeError::Unsupported("u".into()),
			),
			(
				"run container exited with code 1",
				ComposeError::RunExited(1),
			),
			("update error: u", ComposeError::Update("u".into())),
			(
				"external resource not found: external volume \"v\" does not exist",
				ComposeError::ExternalNotFound("external volume \"v\" does not exist".into()),
			),
			(
				"service 'web' publishes fixed host port(s) [8080] but is scaled to 3 replicas",
				ComposeError::ScalePortConflict {
					service: "web".into(),
					replicas: 3,
					ports: vec![8080],
				},
			),
			(
				"container 'web' exited with code 7 while waiting for it to be ready",
				ComposeError::WaitServiceExited {
					container: "web".into(),
					code: 7,
				},
			),
			(
				"service 'web' requests 100000 replicas, which exceeds the limit of 256",
				ComposeError::ReplicaLimitExceeded {
					service: "web".into(),
					replicas: 100_000,
					max: 256,
				},
			),
			(
				"timed out after 30s waiting for services to become healthy",
				ComposeError::WaitTimeout { secs: 30 },
			),
			(
				"service 'web' has no replica 99 (replica indexes are 1-based)",
				ComposeError::ReplicaIndex {
					service: "web".into(),
					index: 99,
				},
			),
			(
				"io error: /out/x.tar:",
				ComposeError::IoPath {
					path: "/out/x.tar".into(),
					source: std::io::Error::other("boom"),
				},
			),
			(
				"build context './ctx' for service 'web':",
				ComposeError::BuildContext {
					service: "web".into(),
					path: "./ctx".into(),
					source: std::io::Error::other("boom"),
				},
			),
			(
				"service 'web' is not running",
				ComposeError::NotRunning("web".into()),
			),
			(
				"exec failed: the exec session did not start within 20s",
				ComposeError::ExecFailed("the exec session did not start within 20s".into()),
			),
			(
				"invalid --timeout -5: use -1 to wait indefinitely or a non-negative number of seconds",
				ComposeError::InvalidTimeout(-5),
			),
			(
				"env file not found: app.env",
				ComposeError::EnvFile("env file not found: app.env".into()),
			),
			(
				"linger is not enabled",
				ComposeError::Autostart("linger is not enabled".into()),
			),
		];
		for (expected_prefix, err) in cases {
			let msg = err.to_string();
			assert!(
				msg.starts_with(expected_prefix),
				"Display for {:?}: got {msg:?}, expected prefix {expected_prefix:?}",
				std::mem::discriminant(err),
			);
		}
	}

	#[test]
	fn parse_display_does_not_echo_offending_scalar() {
		// A type error embeds the offending scalar in the raw serde_yaml message
		// (`invalid type: string "s3cr3t-token", ...`). The Display must not surface
		// that content — it points at the location instead, so a non-compose file
		// pointed at with `-f` cannot leak its bytes onto stderr.
		#[derive(Debug, serde::Deserialize)]
		struct OnlyMap {
			#[allow(dead_code)]
			services: std::collections::BTreeMap<String, String>,
		}
		let err = serde_yaml::from_str::<OnlyMap>("services: s3cr3t-token\n").unwrap_err();
		let msg = ComposeError::Parse(err).to_string();
		assert!(
			!msg.contains("s3cr3t-token"),
			"parse error must not echo file content, got {msg:?}"
		);
		assert!(msg.starts_with("failed to parse compose file"));
	}

	#[test]
	fn source_provided_for_wrapped_variants() {
		use std::error::Error;
		let io = ComposeError::Io(std::io::Error::other("x"));
		assert!(io.source().is_some());
		// Parse and Podman also wrap a lower-level error and expose it.
		let parse =
			ComposeError::Parse(serde_yaml::from_str::<serde_yaml::Value>(":\0").unwrap_err());
		assert!(parse.source().is_some());
		let podman = ComposeError::Podman(crate::libpod::PodmanError::Api {
			status: 500,
			message: "boom".into(),
		});
		assert!(podman.source().is_some());
		let svc = ComposeError::ServiceNotFound("s".into());
		assert!(svc.source().is_none());
	}

	#[test]
	fn service_name_control_chars_are_escaped_in_display() {
		// A crafted name carrying an ESC sequence and newline must not reach the
		// terminal raw: the control bytes are escaped, the quotes preserved.
		let err = ComposeError::ServiceNotFound("we\x1b[31mb\n".into());
		let msg = err.to_string();
		assert!(!msg.contains('\x1b'), "ESC must be escaped: {msg:?}");
		assert!(!msg.contains('\n'), "newline must be escaped: {msg:?}");
		assert!(
			msg.contains("\\u{1b}") && msg.contains("\\n"),
			"got {msg:?}"
		);
	}

	#[test]
	fn replica_index_hint_is_outside_the_quoted_name() {
		// The hint must render after the closing quote, not inside the service name.
		let err = ComposeError::ReplicaIndex {
			service: "web".into(),
			index: 0,
		};
		let msg = err.to_string();
		assert!(msg.contains("'web'"), "service name stays clean: {msg:?}");
		assert!(!msg.contains("'web "), "hint leaked into the name: {msg:?}");
		assert!(msg.contains("1-based"));
	}

	#[test]
	fn from_impls_convert_correctly() {
		let io_err = std::io::Error::other("x");
		let e: ComposeError = io_err.into();
		assert!(matches!(e, ComposeError::Io(_)));

		let yaml_err = serde_yaml::from_str::<serde_yaml::Value>(":\0").unwrap_err();
		let e: ComposeError = yaml_err.into();
		assert!(matches!(e, ComposeError::Parse(_)));

		let podman_err = crate::libpod::PodmanError::Api {
			status: 404,
			message: "not found".into(),
		};
		let e: ComposeError = podman_err.into();
		assert!(matches!(e, ComposeError::Podman(_)));
	}
}
