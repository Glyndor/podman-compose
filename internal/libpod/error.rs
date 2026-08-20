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
	pub fn is_incomplete_message(&self) -> bool {
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
	pub fn stream_end_kind(&self) -> &'static str {
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
	pub fn is_timeout(&self) -> bool {
		matches!(self, Self::Api { status: 0, message } if message.contains("timed out"))
	}

	/// True if this is the libpod 409 returned when `kill` targets a container
	/// that is not running ("can only kill running containers …"). `docker
	/// compose kill` is best-effort across all targets, so this is treated as an
	/// idempotent no-op rather than a fatal error that aborts the loop. The
	/// message is unique to the kill endpoint, so matching it cannot mask another
	/// op's 409.
	pub fn is_kill_of_stopped(&self) -> bool {
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
	pub fn is_already_exists(&self) -> bool {
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
	pub fn is_image_in_use(&self) -> bool {
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
	pub fn is_state_conflict(&self) -> bool {
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
mod tests;
