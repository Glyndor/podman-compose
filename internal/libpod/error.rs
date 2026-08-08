//! Error type for libpod REST API calls.

use std::fmt;

/// Errors from the Podman libpod REST API client.
#[derive(Debug)]
pub enum PodmanError {
	/// I/O or socket connection error.
	Connect(std::io::Error),
	/// Hyper HTTP error.
	Hyper(hyper::Error),
	/// JSON serialization or deserialization error.
	Json(serde_json::Error),
	/// A daemon-controlled stream exceeded the client-side byte limit.
	StreamTooLarge,
	/// A newline-delimited stream ended with an unterminated record.
	StreamEndedEarly,
	/// Podman API returned an error response (4xx/5xx).
	Api { status: u16, message: String },
	/// A libpod field-level rejection that podup identified and attributed to a
	/// specific field of the request it sent.
	///
	/// libpod validates a handful of fields at the `SpecGenerator` (container
	/// create) and build-query layer — namespaces via `ParseNamespace`,
	/// `device_cgroup_rule` access strings via `parseLinuxResourcesDeviceAccess`,
	/// build-arg/label keys, and a few others. When podup pre-validates these,
	/// or when it identifies the offending field by inspecting the request it
	/// just sent, it surfaces the rejection as a `Field` so the user sees the
	/// compose-side key plus the offending value, not the raw libpod message.
	/// `service` is the compose service name for `SpecGenerator` requests and
	/// the empty string for build-query requests (which have no service
	/// context); `field` is the compose-side field name (e.g. `"pid"`,
	/// `"build.args"`); `value` is the offending value as it was sent;
	/// `message` is the ready-to-print explanation and includes the libpod
	/// detail so the cause is not lost (#1357).
	Field {
		/// Compose service name, or `""` when the request had no service
		/// context (build queries, ping).
		service: String,
		/// Compose-side field name (e.g. `"pid"`, `"runtime"`, `"build.args"`).
		field: String,
		/// The offending value, truncated if necessary so a huge or binary
		/// value does not flood the error.
		value: String,
		/// Ready-to-print explanation; carries the libpod detail so the cause
		/// is preserved.
		message: String,
	},
	/// The reachable Podman server speaks a libpod API version below the minimum
	/// podup supports. Carries the version string the server reported (empty when
	/// the server sent no `Libpod-API-Version` header).
	IncompatibleApiVersion { reported: String },
}

impl fmt::Display for PodmanError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Connect(e) => write!(f, "podman socket connection error: {e}"),
			Self::Hyper(e) => write!(f, "http error: {e}"),
			Self::Json(e) => write!(f, "json error: {e}"),
			Self::StreamTooLarge => write!(f, "stream exceeds the 1048576 byte limit"),
			Self::StreamEndedEarly => write!(f, "stream ended early"),
			Self::Api { status, message } => match conflict_hint(message) {
				Some(hint) => write!(f, "{hint} (podman: {message})"),
				None => write!(f, "podman API error (HTTP {status}): {message}"),
			},
			Self::Field {
				service,
				field,
				value,
				message,
			} => {
				if service.is_empty() {
					write!(f, "{field}: {message} (value: {value})")
				} else {
					write!(f, "service.{service}: {field}: {message} (value: {value})")
				}
			}
			Self::IncompatibleApiVersion { reported } => {
				let reported = if reported.is_empty() {
					"an unknown version"
				} else {
					reported.as_str()
				};
				write!(
					f,
					"podup requires Podman >= 5.0; this server reports libpod API version {reported}"
				)
			}
		}
	}
}

/// A short, actionable hint for the common Podman container state-conflict
/// errors, so the CLI leads with plain guidance instead of the raw HTTP message
/// (which still follows in parentheses). Returns `None` for anything unrecognised
/// so the original message is shown verbatim. Pure, so it is unit-tested.
fn conflict_hint(message: &str) -> Option<&'static str> {
	let m = message.to_ascii_lowercase();
	if m.contains("without force") || (m.contains("cannot remove") && m.contains("running")) {
		// Podman's removal refusal wording is the same for a running and a paused
		// container ("running or paused containers cannot be removed without
		// force"); the state is only in the leading "as it is paused/running"
		// clause. Match that so a paused container is not mislabelled as running.
		if m.contains("is paused") {
			Some("the container is paused — unpause it first, or pass `-f` to force removal")
		} else {
			Some("the container is running — stop it first, or pass `-f` to force removal")
		}
	} else if m.contains("already paused") {
		Some("the container is already paused")
	} else if m.contains("not paused") {
		Some("the container is not paused")
	} else if m.contains("not running")
		|| m.contains("can only kill running containers")
		|| m.contains("can only create exec sessions on running containers")
	{
		// kill/exec against a stopped container, plus the generic "not running".
		Some("the container is not running")
	} else if m.contains("already running") {
		Some("the container is already running")
	} else if m.contains("must be in created or stopped state")
		|| (m.contains("unable to start") && m.contains("state"))
	{
		// start of a container that is not in a startable state (e.g. paused).
		Some("the container cannot be started in its current state")
	} else if m.contains("container state improper") {
		// restart/other ops that podman rejects with the generic state message.
		Some("the container is not in a valid state for this operation")
	} else {
		None
	}
}

impl std::error::Error for PodmanError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Connect(e) => Some(e),
			Self::Hyper(e) => Some(e),
			Self::Json(e) => Some(e),
			Self::StreamTooLarge
			| Self::StreamEndedEarly
			| Self::Api { .. }
			| Self::Field { .. }
			| Self::IncompatibleApiVersion { .. } => None,
		}
	}
}

impl From<std::io::Error> for PodmanError {
	fn from(e: std::io::Error) -> Self {
		Self::Connect(e)
	}
}

impl From<hyper::Error> for PodmanError {
	fn from(e: hyper::Error) -> Self {
		Self::Hyper(e)
	}
}

impl From<serde_json::Error> for PodmanError {
	fn from(e: serde_json::Error) -> Self {
		Self::Json(e)
	}
}

/// Whether an API error has the given HTTP status code.
impl PodmanError {
	/// True if this is an API error carrying the given HTTP status code.
	pub fn is_status(&self, code: u16) -> bool {
		matches!(self, Self::Api { status, .. } if *status == code)
	}

	/// True when the server closed the connection before completing the HTTP
	/// response (hyper's `IncompleteMessage`). The container-archive PUT on
	/// Podman 6 applies the archive and then closes without a response, so `cp`
	/// uses this to tell that specific case apart and re-verify the copy landed
	/// rather than failing an upload that actually succeeded (#1097).
	pub(crate) fn is_incomplete_message(&self) -> bool {
		matches!(self, Self::Hyper(e) if e.is_incomplete_message())
	}

	/// How a streaming read ended, for the one question the *transport* cannot
	/// answer: was the stream *finished* or *broken*?
	///
	/// The parsers return `Ok(None)` on a clean body end, so in principle every
	/// `Err` is a fault. In practice it is not, and the two cases are not
	/// separable here: a body cut between chunks and one cut mid-payload arrive
	/// alike, and a finished stream whose terminator went missing is
	/// indistinguishable from either (#1104, pinned by `stream_end_tests`).
	///
	/// Every streaming command therefore answers it out of band instead, by
	/// asking something the transport cannot see — whether the container it was
	/// following is still running, or whether the caller asked for a bounded
	/// stream at all.
	///
	/// This names the hyper classification so a log can say *which* ending
	/// occurred. It stays diagnostic: nothing branches on it.
	pub(crate) fn stream_end_kind(&self) -> &'static str {
		match self {
			Self::Hyper(e) if e.is_incomplete_message() => "incomplete-message",
			Self::Hyper(e) if e.is_body_write_aborted() => "body-write-aborted",
			Self::Hyper(e) if e.is_canceled() => "canceled",
			Self::Hyper(e) if e.is_closed() => "closed",
			Self::Hyper(e) if e.is_timeout() => "hyper-timeout",
			// A body that stopped short is none of the above. `is_incomplete_message`
			// is about the message head, so a severed *body* misses every predicate
			// hyper exposes and used to land in `hyper-other` — the least
			// informative label, for the likeliest shape.
			//
			// Measured against a fake socket that cuts a chunked body deliberately
			// (`engine::stream_end_tests`), both places a cut can land arrive as a
			// hyper Body error wrapping `io::ErrorKind::UnexpectedEof`:
			//
			//   between chunks  "unexpected EOF during chunk size line"
			//   mid-payload     IncompleteBody
			//
			// The kind is what this keys on; hyper's message text distinguishing the
			// two is not something to depend on.
			Self::Hyper(e) if body_ended_early(e) => "body-unexpected-eof",
			Self::Hyper(_) => "hyper-other",
			Self::Connect(e) => match e.kind() {
				std::io::ErrorKind::UnexpectedEof => "io-unexpected-eof",
				std::io::ErrorKind::ConnectionReset => "io-connection-reset",
				std::io::ErrorKind::BrokenPipe => "io-broken-pipe",
				_ => "io-other",
			},
			Self::Json(_) => "malformed-frame",
			Self::StreamTooLarge => "stream-too-large",
			Self::StreamEndedEarly => "stream-ended-early",
			Self::Field { .. } => "field-rejected",
			_ => "other",
		}
	}

	/// True if this is a client-side timeout (the request was aborted because the
	/// socket never responded within the deadline). These carry a synthetic
	/// status `0` and a `timed out` message; lifecycle callers use this to
	/// escalate a wedged `stop` to an explicit `SIGKILL`.
	pub(crate) fn is_timeout(&self) -> bool {
		matches!(self, Self::Api { status: 0, message } if message.contains("timed out"))
	}

	/// True if this is the libpod 409 returned when `kill` targets a container
	/// that is not running ("can only kill running containers …"). `docker
	/// compose kill` is best-effort across all targets, so this is treated as an
	/// idempotent no-op rather than a fatal error that aborts the loop. The
	/// message is unique to the kill endpoint, so matching it cannot mask another
	/// op's 409.
	pub(crate) fn is_kill_of_stopped(&self) -> bool {
		matches!(
			self,
			Self::Api { status: 409, message }
				if message.to_ascii_lowercase().contains("can only kill running")
		)
	}

	/// True if this API error reports that the resource already exists: an HTTP
	/// 409 conflict, or an HTTP 500 whose message says so. Podman's libpod
	/// volume-create endpoint returns 500 (not 409) for a duplicate name, so an
	/// idempotent create must accept both to let a re-`up` succeed.
	pub(crate) fn is_already_exists(&self) -> bool {
		match self {
			Self::Api { status: 409, .. } => true,
			Self::Api {
				status: 500,
				message,
			} => message.contains("already exists"),
			_ => false,
		}
	}

	/// True if this API error reports the target image is still referenced by a
	/// container. A non-force `down --rmi` must skip such an image (matching
	/// docker compose) instead of force-removing it and cascading the deletion of
	/// every dependent container — including ones owned by other projects. Podman
	/// returns this as a 409 conflict, or on some versions a 500 whose message
	/// names the in-use cause.
	pub(crate) fn is_image_in_use(&self) -> bool {
		match self {
			Self::Api { status: 409, .. } => true,
			Self::Api {
				status: 500,
				message,
			} => {
				let m = message.to_ascii_lowercase();
				m.contains("in use") || m.contains("being used") || m.contains("used by")
			}
			_ => false,
		}
	}

	/// True if this API error reports a container is in the wrong state for the
	/// attempted lifecycle op (already paused, not paused, not running). Podman
	/// returns these as a 409/500 with a "container state improper" cause. Lets
	/// `pause`/`unpause` stay idempotent no-ops, matching docker compose.
	pub(crate) fn is_state_conflict(&self) -> bool {
		match self {
			Self::Api { status, message } if *status == 409 || *status == 500 => {
				let m = message.to_ascii_lowercase();
				m.contains("state improper")
					|| m.contains("already paused")
					|| m.contains("not paused")
					|| m.contains("not running")
			}
			_ => false,
		}
	}
}

/// Whether a hyper error is a body that ended before it was complete — the shape
/// a severed stream takes, which none of hyper's own predicates report. Walks the
/// source chain for an `io::Error` of kind `UnexpectedEof` rather than matching on
/// message text.
fn body_ended_early(e: &hyper::Error) -> bool {
	let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
	while let Some(current) = source {
		if let Some(io) = current.downcast_ref::<std::io::Error>() {
			if io.kind() == std::io::ErrorKind::UnexpectedEof {
				return true;
			}
		}
		source = std::error::Error::source(current);
	}
	false
}

#[cfg(test)]
mod tests {
	use super::{conflict_hint, PodmanError};

	#[test]
	fn conflict_hint_recognises_common_state_errors() {
		let rm = "cannot remove container abc as it is running - running or paused containers \
			cannot be removed without force: container state improper";
		assert!(conflict_hint(rm).unwrap().contains("-f"));
		assert!(conflict_hint("container abc is already paused")
			.unwrap()
			.contains("already paused"));
		assert!(conflict_hint("container abc is not paused")
			.unwrap()
			.contains("not paused"));
		assert!(
			conflict_hint("cannot kill container abc: container abc is not running")
				.unwrap()
				.contains("not running")
		);
		// libpod's kill-of-stopped 409 message ("can only kill running
		// containers …") gets the same friendly "not running" hint.
		assert!(conflict_hint(
			"can only kill running containers. abc is in state exited: container state improper"
		)
		.unwrap()
		.contains("not running"));
	}

	#[test]
	fn is_kill_of_stopped_matches_only_the_kill_409() {
		let stopped = PodmanError::Api {
			status: 409,
			message: "can only kill running containers. abc is in state exited: \
				container state improper"
				.into(),
		};
		assert!(stopped.is_kill_of_stopped());
		// A different 409 (e.g. already-paused) must not be swallowed by kill.
		let paused = PodmanError::Api {
			status: 409,
			message: "container abc is already paused".into(),
		};
		assert!(!paused.is_kill_of_stopped());
		// Wrong status, even with a matching message, is not a kill-of-stopped.
		let other = PodmanError::Api {
			status: 500,
			message: "can only kill running containers".into(),
		};
		assert!(!other.is_kill_of_stopped());
		assert!(
			!PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err()).is_kill_of_stopped()
		);
	}

	#[test]
	fn conflict_hint_paused_rm_is_not_labelled_running() {
		// Podman's removal refusal for a *paused* container shares the "without
		// force" wording; the hint must say paused, not running.
		let paused = "cannot remove container abc as it is paused - running or paused containers \
			cannot be removed without force: container state improper";
		let hint = conflict_hint(paused).unwrap();
		assert!(hint.contains("paused"), "got {hint:?}");
		assert!(!hint.contains("running"), "must not say running: {hint:?}");
		assert!(hint.contains("-f"));
	}

	#[test]
	fn conflict_hint_covers_kill_exec_and_start() {
		// kill a stopped container.
		assert!(
			conflict_hint("can only kill running containers. abc is in state exited")
				.unwrap()
				.contains("not running")
		);
		// exec into a stopped container.
		assert!(conflict_hint(
			"can only create exec sessions on running containers: container state improper"
		)
		.unwrap()
		.contains("not running"));
		// start a container that is not startable.
		assert!(conflict_hint(
			"unable to start container abc: container must be in Created or Stopped state to be \
			 started"
		)
		.unwrap()
		.contains("cannot be started"));
	}

	#[test]
	fn conflict_hint_none_for_unrecognised() {
		assert!(conflict_hint("some unrelated error").is_none());
		assert!(conflict_hint("no such container: abc").is_none());
	}

	#[test]
	fn api_error_display_leads_with_hint_and_keeps_message() {
		let e = PodmanError::Api {
			status: 409,
			message: "container abc is already paused".into(),
		};
		let s = e.to_string();
		assert!(s.starts_with("the container is already paused"));
		assert!(s.contains("podman: container abc is already paused"));
	}

	#[test]
	fn api_error_display_raw_when_no_hint() {
		let e = PodmanError::Api {
			status: 500,
			message: "boom".into(),
		};
		assert_eq!(e.to_string(), "podman API error (HTTP 500): boom");
	}

	#[test]
	fn is_status_matches_code() {
		let e = PodmanError::Api {
			status: 404,
			message: "not found".into(),
		};
		assert!(e.is_status(404));
		assert!(!e.is_status(200));
		assert!(!e.is_status(500));
	}

	#[test]
	fn is_timeout_matches_synthetic_timeout_error() {
		// Client-side timeouts carry status 0 and a "timed out" message; lifecycle
		// stop escalation keys off this.
		assert!(PodmanError::Api {
			status: 0,
			message: "timed out after 40s waiting for the Podman socket to respond".into(),
		}
		.is_timeout());
		// A real HTTP error (non-zero status) is not a timeout.
		assert!(!PodmanError::Api {
			status: 500,
			message: "boom".into(),
		}
		.is_timeout());
		// A status-0 error without the timeout marker is not a timeout.
		assert!(!PodmanError::Api {
			status: 0,
			message: "invalid API path".into(),
		}
		.is_timeout());
	}

	#[test]
	fn is_status_false_for_non_api() {
		let e = PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err());
		assert!(!e.is_status(404));
	}

	#[test]
	fn already_exists_accepts_409_and_500_with_message() {
		// 409 conflict: always an already-exists.
		assert!(PodmanError::Api {
			status: 409,
			message: "network already used".into(),
		}
		.is_already_exists());
		// 500 carrying the libpod "already exists" cause (Podman's volume-create
		// path) must also count as already-exists for idempotent create.
		assert!(PodmanError::Api {
			status: 500,
			message: "volume with name p_v already exists: volume already exists".into(),
		}
		.is_already_exists());
	}

	#[test]
	fn image_in_use_accepts_409_and_500_with_message() {
		// A 409 on image delete is always an in-use conflict.
		assert!(PodmanError::Api {
			status: 409,
			message: "image is in use by 1 container".into(),
		}
		.is_image_in_use());
		// Some Podman versions report it as a 500 naming the cause.
		assert!(PodmanError::Api {
			status: 500,
			message: "image used by a container: image in use".into(),
		}
		.is_image_in_use());
		// An unrelated 500 still propagates.
		assert!(!PodmanError::Api {
			status: 500,
			message: "internal error".into(),
		}
		.is_image_in_use());
		assert!(!PodmanError::Api {
			status: 404,
			message: "no such image".into(),
		}
		.is_image_in_use());
	}

	#[test]
	fn state_conflict_recognises_pause_unpause_mismatches() {
		for msg in [
			"container abc is already paused: container state improper",
			"container abc is not paused: container state improper",
			"cannot pause container abc: container abc is not running",
			"unpausing container: container state improper",
		] {
			assert!(
				PodmanError::Api {
					status: 500,
					message: msg.into(),
				}
				.is_state_conflict(),
				"should treat {msg:?} as a state conflict"
			);
		}
		// A genuine failure is not a state conflict.
		assert!(!PodmanError::Api {
			status: 500,
			message: "internal error".into(),
		}
		.is_state_conflict());
		assert!(!PodmanError::Api {
			status: 404,
			message: "no such container".into(),
		}
		.is_state_conflict());
	}

	#[test]
	fn already_exists_false_for_other_errors() {
		// A 500 that is not an already-exists must still propagate.
		assert!(!PodmanError::Api {
			status: 500,
			message: "internal error".into(),
		}
		.is_already_exists());
		assert!(!PodmanError::Api {
			status: 404,
			message: "no such volume".into(),
		}
		.is_already_exists());
		assert!(
			!PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err()).is_already_exists()
		);
	}

	#[test]
	fn display_api_error() {
		let e = PodmanError::Api {
			status: 500,
			message: "internal error".into(),
		};
		assert_eq!(e.to_string(), "podman API error (HTTP 500): internal error");
	}

	#[test]
	fn display_json_error() {
		let e = PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err());
		assert!(e.to_string().contains("json error"));
	}

	#[test]
	fn display_connect_error() {
		let e = PodmanError::Connect(std::io::Error::new(
			std::io::ErrorKind::NotFound,
			"no socket",
		));
		assert!(e.to_string().contains("podman socket connection error"));
	}

	#[test]
	fn display_incompatible_api_version_reports_version() {
		let e = PodmanError::IncompatibleApiVersion {
			reported: "4.9.3".into(),
		};
		let msg = e.to_string();
		assert!(msg.contains("Podman >= 5.0"));
		assert!(msg.contains("4.9.3"));
	}

	#[test]
	fn display_incompatible_api_version_handles_missing_header() {
		// An empty reported version (no `Libpod-API-Version` header) renders a
		// readable placeholder rather than a blank.
		let e = PodmanError::IncompatibleApiVersion {
			reported: String::new(),
		};
		let msg = e.to_string();
		assert!(msg.contains("an unknown version"));
	}

	#[test]
	fn source_present_for_wrapped_errors_absent_for_owned() {
		use std::error::Error;
		// Wrapped lower-level errors expose their source...
		let connect = PodmanError::Connect(std::io::Error::new(
			std::io::ErrorKind::NotFound,
			"no socket",
		));
		assert!(connect.source().is_some());
		let json = PodmanError::Json(serde_json::from_str::<u8>("bad").unwrap_err());
		assert!(json.source().is_some());
		// ...while the owned variants have no underlying source.
		assert!(PodmanError::Api {
			status: 500,
			message: "x".into(),
		}
		.source()
		.is_none());
		assert!(PodmanError::IncompatibleApiVersion {
			reported: "4.0.0".into(),
		}
		.source()
		.is_none());
	}

	#[test]
	fn from_io_error_becomes_connect() {
		// The `?`-operator conversion an io error takes when bubbling out of the
		// client maps onto the Connect variant.
		let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
		let e: PodmanError = io.into();
		assert!(matches!(e, PodmanError::Connect(_)));
		assert!(e.to_string().contains("podman socket connection error"));
	}

	#[test]
	fn from_json_error_becomes_json() {
		let json_err = serde_json::from_str::<u8>("not-json").unwrap_err();
		let e: PodmanError = json_err.into();
		assert!(matches!(e, PodmanError::Json(_)));
		assert!(e.to_string().contains("json error"));
	}

	#[test]
	fn field_error_names_service_and_field() {
		// A `Field` rendered for a container-create failure leads with the
		// service and field, then the value, so the operator sees the
		// offending entry without parsing the libpod body (#1357).
		let e = PodmanError::Field {
			service: "web".into(),
			field: "pid".into(),
			value: "evil".into(),
			message: "namespace not recognised".into(),
		};
		let msg = e.to_string();
		assert!(
			msg.starts_with("service.web: pid: namespace not recognised"),
			"got {msg:?}"
		);
		assert!(msg.contains("evil"));
	}

	#[test]
	fn field_error_for_build_query_skips_service_prefix() {
		// Build queries have no service context, so the `service.X:` prefix
		// is omitted and the field name leads directly.
		let e = PodmanError::Field {
			service: String::new(),
			field: "build.args".into(),
			value: "MALFORMED\\nKEY".into(),
			message: "key is not a valid identifier".into(),
		};
		let msg = e.to_string();
		assert!(
			msg.starts_with("build.args: key is not a valid identifier"),
			"got {msg:?}"
		);
		assert!(!msg.contains("service."));
		assert!(msg.contains("MALFORMED\\nKEY"));
	}

	#[test]
	fn field_error_has_no_source() {
		// Like the other owned variants, `Field` has no underlying error to
		// chain — the explanation is in the variant's own fields.
		use std::error::Error;
		let e = PodmanError::Field {
			service: "web".into(),
			field: "pid".into(),
			value: "evil".into(),
			message: "namespace not recognised".into(),
		};
		assert!(e.source().is_none());
	}

	#[test]
	fn is_status_does_not_match_field() {
		// The `is_status` predicate keys on the `Api` variant, so a `Field`
		// rejection is never mistaken for a generic API error.
		let e = PodmanError::Field {
			service: "web".into(),
			field: "pid".into(),
			value: "evil".into(),
			message: "x".into(),
		};
		assert!(!e.is_status(400));
		assert!(!e.is_status(500));
	}
}
