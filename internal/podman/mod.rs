//! Podman socket connection helpers.

// libc FFI (getuid) is needed here; the block carries a soundness comment.
#![allow(unsafe_code)]

use crate::error::{ComposeError, Result};
use crate::libpod::Client;
#[cfg(any(not(windows), test))]
use std::path::Path;

#[cfg(any(not(windows), test))]
const ROOT_SOCKET: &str = "/run/podman/podman.sock";

/// Named pipe `podman machine` exposes on Windows for its default machine.
#[cfg(windows)]
const DEFAULT_PIPE: &str = "//./pipe/podman-machine-default";

/// Connect to Podman's libpod REST API.
///
/// Priority:
/// 1. `socket_path` if provided.
/// 2. The first existing platform default: on Linux the rootful or
///    per-user runtime socket, on macOS the host-side socket exposed by
///    `podman machine`, on Windows the `podman machine` named pipe.
/// 3. The conventional path for this platform, so a failed connection
///    reports the location podup expected.
pub fn connect(socket_path: Option<&str>) -> Result<Client> {
	connect_with_pool_size(socket_path, Client::DEFAULT_POOL_SIZE)
}

/// As [`connect`], with a caller-chosen HTTP/1.1 connection-pool size. The
/// pool is keyed by socket path, so the cap controls the number of
/// concurrent connections a single [`Client`] will keep open to the socket.
/// `pool_size` is floored at 1 by [`Client::with_pool_size`].
pub fn connect_with_pool_size(socket_path: Option<&str>, pool_size: usize) -> Result<Client> {
	let default_path = default_socket_path();
	let raw = socket_path.unwrap_or(&default_path);
	if let Some(scheme) = remote_scheme(raw) {
		return Err(ComposeError::Unsupported(format!(
			"remote Podman over `{scheme}` is not supported; podup talks to a local \
			 rootless socket. Point PODMAN_SOCKET/--socket at a unix:// socket path \
			 (or an npipe:// pipe on Windows)."
		)));
	}
	let path = raw
		.strip_prefix("unix://")
		.or_else(|| raw.strip_prefix("npipe://"))
		.unwrap_or(raw);
	Ok(Client::with_pool_size(path, pool_size))
}

/// Detect a non-local socket scheme (`tcp://`, `ssh://`, `http(s)://`, `fd://`).
/// `unix://`/`npipe://` and plain paths are local and return `None`.
fn remote_scheme(raw: &str) -> Option<&'static str> {
	const REMOTE: [&str; 5] = ["tcp://", "ssh://", "http://", "https://", "fd://"];
	REMOTE.into_iter().find(|s| raw.starts_with(s))
}

/// Read the Podman socket from the environment (`PODMAN_SOCKET`, then
/// `DOCKER_HOST` as a Docker-compatible fallback) and connect. A `unix://` /
/// `npipe://` scheme is stripped by [`connect`]; a remote scheme is rejected
/// there with a clear error.
pub fn connect_from_env() -> Result<Client> {
	connect_from_env_with_pool_size(Client::DEFAULT_POOL_SIZE)
}

/// As [`connect_from_env`], with a caller-chosen HTTP/1.1 connection-pool
/// size (see [`connect_with_pool_size`]).
pub fn connect_from_env_with_pool_size(pool_size: usize) -> Result<Client> {
	let socket = std::env::var("PODMAN_SOCKET")
		.or_else(|_| std::env::var("DOCKER_HOST"))
		.ok();

	connect_with_pool_size(socket.as_deref(), pool_size)
}

#[cfg(not(windows))]
pub(crate) fn default_socket_path() -> String {
	let candidates = candidate_socket_paths();
	first_existing(&candidates)
		.or_else(machine_socket_path)
		.or_else(|| candidates.into_iter().next())
		.unwrap_or_else(|| ROOT_SOCKET.to_string())
}

/// Windows: named pipes are not probeable through `Path::exists`, so ask
/// `podman machine inspect` and fall back to the default machine's pipe.
#[cfg(windows)]
pub(crate) fn default_socket_path() -> String {
	machine_socket_path().unwrap_or_else(|| DEFAULT_PIPE.to_string())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn candidate_socket_paths() -> Vec<String> {
	// SAFETY: getuid takes no arguments, touches no memory and cannot fail.
	let uid = unsafe { libc::getuid() };
	runtime_candidates(uid, std::env::var("XDG_RUNTIME_DIR").ok().as_deref())
}

#[cfg(target_os = "macos")]
fn candidate_socket_paths() -> Vec<String> {
	match std::env::var("HOME") {
		Ok(home) => machine_candidates(&home),
		Err(_) => vec![ROOT_SOCKET.to_string()],
	}
}

/// Socket candidates for Linux and other unix hosts: the rootful socket
/// for uid 0, otherwise the user's runtime directory (preferring
/// `XDG_RUNTIME_DIR` when set).
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn runtime_candidates(uid: u32, xdg_runtime_dir: Option<&str>) -> Vec<String> {
	if uid == 0 {
		return vec![ROOT_SOCKET.to_string()];
	}
	let mut candidates = Vec::new();
	if let Some(dir) = xdg_runtime_dir {
		if !dir.is_empty() {
			candidates.push(format!("{dir}/podman/podman.sock"));
		}
	}
	let run_user = format!("/run/user/{uid}/podman/podman.sock");
	if !candidates.contains(&run_user) {
		candidates.push(run_user);
	}
	candidates
}

/// Socket candidates on macOS: the host-side sockets `podman machine`
/// creates, newest layout first. Podman 5 names the per-provider directory
/// after the active machine provider (`applehv` by default, `vz` for the
/// Virtualization.framework backend), so those are tried before the older
/// `qemu`/default layouts.
#[cfg(any(target_os = "macos", test))]
fn machine_candidates(home: &str) -> Vec<String> {
	let machine_dir = format!("{home}/.local/share/containers/podman/machine");
	vec![
		format!("{machine_dir}/podman.sock"),
		format!("{machine_dir}/applehv/podman.sock"),
		format!("{machine_dir}/vz/podman.sock"),
		format!("{machine_dir}/qemu/podman.sock"),
		format!("{machine_dir}/podman-machine-default/podman.sock"),
	]
}

/// Ask `podman machine inspect` for the host-side socket path. Only used
/// on macOS, where the VM provider decides where the API socket lives.
#[cfg(target_os = "macos")]
fn machine_socket_path() -> Option<String> {
	let output = std::process::Command::new("podman")
		.args([
			"machine",
			"inspect",
			"--format",
			"{{ .ConnectionInfo.PodmanSocket.Path }}",
		])
		.output()
		.ok()?;
	if !output.status.success() {
		return None;
	}
	let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
	(!path.is_empty() && Path::new(&path).exists()).then_some(path)
}

/// Ask `podman machine inspect` for the named pipe of the default machine.
/// The pipe path is reported by the machine config; `Path::exists` cannot
/// probe named pipes, so the value is used as-is.
#[cfg(windows)]
fn machine_socket_path() -> Option<String> {
	let output = std::process::Command::new("podman")
		.args([
			"machine",
			"inspect",
			"--format",
			"{{ .ConnectionInfo.PodmanPipe.Path }}",
		])
		.output()
		.ok()?;
	if !output.status.success() {
		return None;
	}
	let path = String::from_utf8(output.stdout).ok()?.trim().to_string();
	(!path.is_empty() && path != "<nil>").then_some(path)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn machine_socket_path() -> Option<String> {
	None
}

#[cfg(any(not(windows), test))]
fn first_existing(candidates: &[String]) -> Option<String> {
	candidates.iter().find(|p| Path::new(p).exists()).cloned()
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
