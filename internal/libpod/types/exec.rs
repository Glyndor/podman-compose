//! Podman libpod exec API request and response types.

use serde::{Deserialize, Serialize};

/// Request body for `POST /libpod/containers/{name}/exec`.
#[derive(Serialize, Default)]
pub struct ExecCreateConfig {
	/// Command and arguments to run, as an argv vector (not shell-parsed).
	#[serde(rename = "Cmd", skip_serializing_if = "Option::is_none")]
	pub cmd: Option<Vec<String>>,

	/// Whether to attach to the exec process's stdout.
	#[serde(rename = "AttachStdout", skip_serializing_if = "Option::is_none")]
	pub attach_stdout: Option<bool>,

	/// Whether to attach to the exec process's stderr.
	#[serde(rename = "AttachStderr", skip_serializing_if = "Option::is_none")]
	pub attach_stderr: Option<bool>,

	/// Whether to attach to the exec process's stdin.
	#[serde(rename = "AttachStdin", skip_serializing_if = "Option::is_none")]
	pub attach_stdin: Option<bool>,

	/// Whether to allocate a pseudo-TTY for the exec process.
	#[serde(rename = "Tty", skip_serializing_if = "Option::is_none")]
	pub tty: Option<bool>,

	/// User (and optionally group) to run the exec process as (`user[:group]`).
	#[serde(rename = "User", skip_serializing_if = "Option::is_none")]
	pub user: Option<String>,

	/// Whether to run the exec process with extended (privileged) permissions.
	#[serde(rename = "Privileged", skip_serializing_if = "Option::is_none")]
	pub privileged: Option<bool>,

	/// Working directory inside the container for the exec process.
	#[serde(rename = "WorkingDir", skip_serializing_if = "Option::is_none")]
	pub working_dir: Option<String>,

	/// Environment variables for the exec process, each entry `KEY=VALUE`.
	#[serde(rename = "Env", skip_serializing_if = "Option::is_none")]
	pub env: Option<Vec<String>>,
}

/// Response from `POST /libpod/containers/{name}/exec`.
#[derive(Deserialize)]
pub struct ExecCreateResponse {
	/// Exec session ID, used to start and inspect the session.
	#[serde(rename = "Id")]
	pub id: String,
}

/// Request body for `POST /libpod/exec/{id}/start`.
#[derive(Serialize)]
pub struct ExecStartConfig {
	/// Whether to start the exec session detached rather than streaming output.
	#[serde(rename = "Detach")]
	pub detach: bool,

	/// Whether the exec session was created with a TTY; selects raw vs.
	/// multiplexed stream framing on the start response.
	#[serde(rename = "Tty")]
	pub tty: bool,
}

/// Response from `GET /libpod/exec/{id}/json`.
#[derive(Deserialize, Default)]
pub struct ExecInspect {
	/// Exit code of the finished exec process; `None` while it is still running.
	#[serde(rename = "ExitCode")]
	pub exit_code: Option<i64>,
}

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
