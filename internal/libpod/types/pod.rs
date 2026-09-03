//! Podman libpod pod API request and response types.
//!
//! Wire shape is the libpod `PodSpecGenerator` (create) and `PodInspect`
//! (get). The field names match the JSON libpod sends and accepts, with
//! `#[serde(rename = ...)]` only where the Rust name diverges from the wire.

use std::collections::HashMap;

use crate::libpod::types::container::Namespace;

use serde::{Deserialize, Serialize};

use super::container::{PerNetworkOptions, PortMapping};

/// Request body for `POST /libpod/pods/create`.
///
/// Mirrors libpod's `PodSpecGenerator`. The infra container Podman creates
/// inside the pod carries every network namespace of the project plus the
/// published ports, so the wire fields are what the engine actually has to
/// express: a name, a label set (the engine's `podup.project` and the
/// `podup.pod-config-hash` it uses to decide between recreate and reuse),
/// the namespace modes it asks Podman to share (network only; UTS and IPC
/// stay per container so `hostname:` keeps working as today), the union of
/// every service's `portmappings`, the list of networks to attach the infra
/// container to, and the `/etc/hosts` entries that make sibling service
/// names resolve on the shared namespace.
#[derive(Serialize, Default)]
pub struct PodSpecGenerator {
	/// Pod name; matches the project name by convention.
	pub name: String,

	/// Pod labels. The engine stamps `podup.project=<project>` and
	/// `podup.pod-config-hash=<hash>` onto every pod it creates.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub labels: HashMap<String, String>,

	/// Namespaces the infra container shares with the joined containers.
	/// The engine asks for `["net"]` only: UTS and IPC stay per container so
	/// `hostname:` keeps working as it does on a project network.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub shared_namespaces: Vec<String>,

	/// Host-port mappings the infra container publishes. Containers inside a
	/// pod cannot publish ports themselves; the union of every service's
	/// `ports:` lands here, in the same shape the `SpecGenerator.portmappings`
	/// field uses, so the wire field is the same Rust type.
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub portmappings: Vec<PortMapping>,

	/// Networks the infra container attaches to. Keyed by network name; the
	/// inner `PerNetworkOptions` is the same one the container spec uses.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub networks: HashMap<String, PerNetworkOptions>,

	/// Network namespace mode of the infra container. libpod refuses a pod
	/// that names `networks` without `bridge` here ("networks and static
	/// ip/mac address can only be used with Bridge mode networking"), so the
	/// builder sets it whenever the pod attaches to a network.
	#[serde(skip_serializing_if = "Option::is_none", default)]
	pub netns: Option<Namespace>,

	/// `/etc/hosts` entries the infra container carries, so each service name
	/// resolves to the shared network namespace the way it resolves on a
	/// compose project network. Format is the same `host:ip` shape the
	/// container spec's `hostadd` uses (a `Vec<String>` of `<host>:<ip>`).
	#[serde(skip_serializing_if = "Vec::is_empty", default)]
	pub hostadd: Vec<String>,

	/// User namespace of the pod, shared by every member. Podman's CLI
	/// refuses `--userns` on a container inside a pod; the namespace is the
	/// pod's, so a project's common `userns_mode` lands here.
	#[serde(skip_serializing_if = "Option::is_none", default)]
	pub userns: Option<Namespace>,
}

/// Response from `GET /libpod/pods/{name}/json`. Only the fields the engine
/// reads (the labels) are typed; the rest of libpod's payload is captured
/// as raw JSON so a future field the engine starts reading does not need a
/// struct bump.
#[derive(Deserialize, Default)]
pub struct PodInspect {
	/// Pod labels. The engine reads `podup.pod-config-hash` off this map to
	/// decide between recreate and reuse.
	#[serde(rename = "Labels", default)]
	pub labels: HashMap<String, String>,
}

#[cfg(test)]
#[path = "pod_tests.rs"]
mod tests;
