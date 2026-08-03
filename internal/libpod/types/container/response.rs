//! Response types for container API calls.

use std::collections::HashMap;

use serde::Deserialize;

/// Deserialize a collection field, treating JSON `null` as the default.
///
/// Podman sometimes returns `null` instead of `[]`/`{}` for empty collection
/// fields, which would otherwise fail to deserialize into a `Vec`/`HashMap`.
fn null_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
	D: serde::Deserializer<'de>,
	T: Default + Deserialize<'de>,
{
	Option::<T>::deserialize(d).map(|v| v.unwrap_or_default())
}

/// Entry in the `GET /libpod/containers/json` response array.
#[derive(Deserialize)]
pub struct ContainerListEntry {
	/// Full 64-hex container ID.
	#[serde(rename = "Id", default)]
	pub id: String,

	/// Container names, each leading-slash-prefixed as Podman reports them
	/// (e.g. `/web`). A container may carry multiple names/aliases.
	#[serde(rename = "Names", default, deserialize_with = "null_default")]
	pub names: Vec<String>,

	/// Image reference the container was created from (name:tag or an ID).
	#[serde(rename = "Image", default)]
	pub image: String,

	/// Human-readable status string for `ps` display (e.g. `Up 3 minutes`,
	/// `Exited (0) 5 seconds ago`). Empty on libpod, which reports `State`.
	#[serde(rename = "Status", default)]
	pub status: String,

	/// Machine-readable state (`running`, `exited`, `created`, …). Podman's
	/// libpod list response leaves `Status` empty and reports the state here.
	#[serde(rename = "State", default)]
	pub state: String,

	/// Published port mappings for the container.
	#[serde(rename = "Ports", default, deserialize_with = "null_default")]
	pub ports: Vec<ContainerPort>,

	/// Exit code of the last run, present once the container has exited. libpod's
	/// list endpoint reports it (0 while running/created), letting `ps`
	/// distinguish a clean exit from a crash without a follow-up inspect.
	#[serde(rename = "ExitCode", default)]
	pub exit_code: Option<i32>,

	/// Container labels (key/value), including compose project/service labels.
	#[serde(rename = "Labels", default, deserialize_with = "null_default")]
	pub labels: HashMap<String, String>,
	/// When the container was last started, in Unix seconds. Zero for one that
	/// has never run, which is why `ps` keys "how long has this been up" on the
	/// state rather than on this being non-zero.
	#[serde(rename = "StartedAt", default)]
	pub started_at: i64,
	/// On-disk size, present only when the request carried `size=true`.
	///
	/// `None` is the ordinary case: libpod leaves it out unless asked, because
	/// producing it means walking the container's writable layer. Callers render
	/// an empty cell rather than a zero, so "not asked" never reads as "empty".
	#[serde(rename = "Size", default)]
	pub size: Option<ContainerSize>,
	/// When the container was created, as an RFC 3339 string.
	///
	/// Not the docker-compatible `CreatedAt`, which libpod also sends and leaves
	/// **empty** — measured on Podman 5.7.0. Reading the familiar name gets a
	/// blank column, the same trap as `Status` versus `State` above.
	#[serde(rename = "Created", default)]
	pub created: String,
}

/// A container's on-disk footprint, as `containers/json?size=true` reports it.
#[derive(Deserialize, Clone, Copy, Debug)]
pub struct ContainerSize {
	/// Bytes the container has written on top of its image.
	#[serde(rename = "rwSize", default)]
	pub rw: u64,
	/// The image's own size. This is what `podman ps -s` prints as `virtual`,
	/// and it is **not** the sum of the two — verified against three containers
	/// whose two readings differ at three significant digits.
	#[serde(rename = "rootFsSize", default)]
	pub root_fs: u64,
}

/// Port mapping entry in container list response.
#[derive(Deserialize, Default)]
pub struct ContainerPort {
	/// Host IP the port is bound to; absent when bound to all interfaces.
	pub host_ip: Option<String>,
	/// Port on the host the mapping is published to; absent when not published.
	pub host_port: Option<u16>,
	/// Port inside the container that traffic is forwarded to.
	pub container_port: u16,
	/// Transport protocol of the mapping (`tcp`/`udp`/`sctp`), surfaced in `ps`.
	#[serde(default)]
	pub protocol: Option<String>,
	/// Number of consecutive ports this entry covers when libpod collapses a
	/// published range into a single record (e.g. `51251-51253->8080-8082` is one
	/// record with `range = 3`). Rendered by `ps` so the full range is shown.
	#[serde(default)]
	pub range: Option<u16>,
}

/// Response from `GET /libpod/containers/{name}/json`.
#[derive(Deserialize, Default)]
pub struct ContainerInspect {
	/// Runtime state (status, exit code, health).
	#[serde(rename = "State")]
	pub state: Option<ContainerState>,

	/// Static configuration the container was created with.
	#[serde(rename = "Config")]
	pub config: Option<ContainerConfig>,

	/// Runtime network settings, including resolved host port bindings.
	#[serde(rename = "NetworkSettings")]
	pub network_settings: Option<NetworkSettings>,
}

/// Container config sub-object from inspect.
#[derive(Deserialize, Default)]
pub struct ContainerConfig {
	/// Effective healthcheck, if any; absent when the image declares none.
	#[serde(rename = "Healthcheck")]
	pub healthcheck: Option<HealthConfig>,
}

impl ContainerConfig {
	/// Whether the container has an effective healthcheck that can report a
	/// `healthy` status. Covers healthchecks inherited from the image as well
	/// as those declared in compose. A `["NONE"]` test means it was disabled.
	pub fn has_healthcheck(&self) -> bool {
		self.healthcheck
			.as_ref()
			.is_some_and(|h| h.test.first().is_some_and(|first| first != "NONE"))
	}
}

/// Effective healthcheck config baked into the container (image or compose).
#[derive(Deserialize, Default)]
pub struct HealthConfig {
	/// Healthcheck command in Docker's `Test` form: the first element is the
	/// mode (`NONE`, `CMD`, or `CMD-SHELL`) and the rest are its arguments.
	/// `["NONE"]` means the healthcheck is explicitly disabled; empty means none.
	#[serde(rename = "Test", default, deserialize_with = "null_default")]
	pub test: Vec<String>,
}

/// Container state sub-object.
#[derive(Deserialize, Default)]
pub struct ContainerState {
	/// Lifecycle status (`running`, `exited`, ...). Read while waiting to detect a
	/// container that exited before becoming ready.
	#[serde(rename = "Status")]
	pub status: Option<String>,

	/// Exit code once the container has exited; used to fail `--wait` when a
	/// no-healthcheck service crashes during the wait.
	#[serde(rename = "ExitCode")]
	pub exit_code: Option<i64>,

	/// Aggregated healthcheck state; absent when the container has no healthcheck.
	#[serde(rename = "Health")]
	pub health: Option<HealthState>,
}

/// Container health state sub-object.
#[derive(Deserialize)]
pub struct HealthState {
	/// Current health status (`healthy`, `unhealthy`, `starting`).
	#[serde(rename = "Status")]
	pub status: Option<String>,
}

/// Network settings sub-object from container inspect.
#[derive(Deserialize, Default)]
pub struct NetworkSettings {
	/// Resolved port bindings keyed by container port spec (`"80/tcp"`). The
	/// value is the list of host bindings, or `None`/empty when the port is
	/// exposed but not published.
	#[serde(rename = "Ports", default)]
	pub ports: HashMap<String, Option<Vec<HostBinding>>>,
}

/// Host port binding from container inspect network settings.
#[derive(Deserialize, Clone)]
pub struct HostBinding {
	/// Host IP the port is bound to; empty/absent means all interfaces.
	#[serde(rename = "HostIp")]
	pub host_ip: Option<String>,

	/// Host port as a string, as Podman reports it in inspect output.
	#[serde(rename = "HostPort")]
	pub host_port: Option<String>,
}

/// Response from `GET /libpod/containers/{name}/top`.
#[derive(Deserialize, Default)]
pub struct TopResponse {
	/// Column headers for the process table (e.g. `PID`, `USER`, `COMMAND`).
	#[serde(rename = "Titles")]
	pub titles: Option<Vec<String>>,

	/// One row per process; each row's columns align with `titles`.
	#[serde(rename = "Processes")]
	pub processes: Option<Vec<Vec<String>>>,
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::libpod::types::container::spec::Namespace;

	// ---------------------------------------------------------------------------
	// Namespace (spec type — tested here to avoid pushing spec.rs over 500 lines)
	// ---------------------------------------------------------------------------

	#[test]
	fn namespace_new_has_no_value() {
		let ns = Namespace::new("host");
		assert_eq!(ns.nsmode, "host");
		assert!(ns.value.is_none());
	}

	#[test]
	fn namespace_container_sets_value() {
		let ns = Namespace::container("other");
		assert_eq!(ns.nsmode, "container");
		assert_eq!(ns.value.as_deref(), Some("other"));
	}

	#[test]
	fn namespace_parse_container_prefix() {
		let ns = Namespace::parse("container:sidecar");
		assert_eq!(ns.nsmode, "container");
		assert_eq!(ns.value.as_deref(), Some("sidecar"));
	}

	#[test]
	fn namespace_parse_plain_mode() {
		let ns = Namespace::parse("host");
		assert_eq!(ns.nsmode, "host");
		assert!(ns.value.is_none());
	}

	// ---------------------------------------------------------------------------
	// Response deserialization
	// ---------------------------------------------------------------------------

	#[test]
	fn container_inspect_deserialize_healthy() {
		let json = r#"{
			"State": {
				"Status": "running",
				"ExitCode": 0,
				"Health": { "Status": "healthy" }
			}
		}"#;
		let ci: ContainerInspect = serde_json::from_str(json).unwrap();
		let state = ci.state.unwrap();
		assert_eq!(state.status.as_deref(), Some("running"));
		assert_eq!(state.exit_code, Some(0));
		assert_eq!(state.health.unwrap().status.as_deref(), Some("healthy"));
	}

	#[test]
	fn container_inspect_missing_fields_default() {
		let json = r#"{}"#;
		let ci: ContainerInspect = serde_json::from_str(json).unwrap();
		assert!(ci.state.is_none());
		assert!(ci.config.is_none());
		assert!(ci.network_settings.is_none());
	}

	#[test]
	fn has_healthcheck_true_for_image_inherited() {
		let json = r#"{
			"Config": { "Healthcheck": { "Test": ["CMD-SHELL", "curl -f http://localhost || exit 1"] } }
		}"#;
		let ci: ContainerInspect = serde_json::from_str(json).unwrap();
		assert!(ci.config.unwrap().has_healthcheck());
	}

	#[test]
	fn has_healthcheck_false_when_disabled_with_none() {
		let json = r#"{ "Config": { "Healthcheck": { "Test": ["NONE"] } } }"#;
		let ci: ContainerInspect = serde_json::from_str(json).unwrap();
		assert!(!ci.config.unwrap().has_healthcheck());
	}

	#[test]
	fn has_healthcheck_false_when_absent() {
		let json = r#"{ "Config": {} }"#;
		let ci: ContainerInspect = serde_json::from_str(json).unwrap();
		assert!(!ci.config.unwrap().has_healthcheck());
	}

	#[test]
	fn has_healthcheck_false_when_test_null() {
		let json = r#"{ "Config": { "Healthcheck": { "Test": null } } }"#;
		let ci: ContainerInspect = serde_json::from_str(json).unwrap();
		assert!(!ci.config.unwrap().has_healthcheck());
	}

	#[test]
	fn top_response_deserialize() {
		let json = r#"{"Titles": ["PID", "CMD"], "Processes": [["1", "bash"]]}"#;
		let tr: TopResponse = serde_json::from_str(json).unwrap();
		assert_eq!(tr.titles.unwrap(), vec!["PID", "CMD"]);
		assert_eq!(tr.processes.unwrap(), vec![vec!["1", "bash"]]);
	}

	#[test]
	fn container_list_entry_default_fields() {
		let json =
			r#"{"Names": ["/mycontainer"], "Image": "nginx", "Status": "running", "Ports": []}"#;
		let entry: ContainerListEntry = serde_json::from_str(json).unwrap();
		assert_eq!(entry.names, vec!["/mycontainer"]);
		assert_eq!(entry.image, "nginx");
		assert_eq!(entry.status, "running");
	}

	#[test]
	fn container_list_entry_null_vec_fields() {
		let json = r#"{"Names": null, "Image": "alpine", "Status": "exited", "Ports": null}"#;
		let entry: ContainerListEntry = serde_json::from_str(json).unwrap();
		assert!(entry.names.is_empty());
		assert!(entry.ports.is_empty());
	}
}
