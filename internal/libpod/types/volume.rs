//! Podman libpod volume API request and response types.

use std::collections::HashMap;

use serde::Serialize;

/// Request body for `POST /libpod/volumes/create`.
#[derive(Serialize, Default)]
pub struct VolumeCreateOptions {
	/// Volume name; omitted to let Podman generate an anonymous-volume name.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub name: Option<String>,

	/// Volume driver (e.g. `local`); the daemon default is used when omitted.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub driver: Option<String>,

	// Podman's VolumeCreateOptions carries no json tags, so its body decodes by
	// Go field name (case-insensitively): the driver-options map is `Options`, not
	// `driver_opts`. Without this rename volume driver_opts (NFS/CIFS/tmpfs) are
	// silently dropped while name/driver/labels still match case-insensitively.
	/// Driver-specific options (e.g. `type`, `device`, `o` for NFS/CIFS/tmpfs
	/// mounts). Serialized as `Options` to match Podman's wire field (see above).
	#[serde(rename = "Options", skip_serializing_if = "HashMap::is_empty", default)]
	pub driver_opts: HashMap<String, String>,

	/// Volume labels (key/value), including compose project/volume labels.
	#[serde(skip_serializing_if = "HashMap::is_empty", default)]
	pub labels: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
	use super::VolumeCreateOptions;

	#[test]
	fn driver_opts_serialize_as_options_not_driver_opts() {
		let mut opts = VolumeCreateOptions {
			driver: Some("local".to_string()),
			..Default::default()
		};
		opts.driver_opts
			.insert("type".to_string(), "nfs".to_string());
		let v = serde_json::to_value(&opts).unwrap();
		// Podman's VolumeCreateOptions has no json tag on the options map, so the
		// wire key must be `Options`; `driver_opts` would be silently dropped.
		assert!(v.get("Options").is_some(), "expected Options key: {v}");
		assert!(v.get("driver_opts").is_none(), "stale driver_opts key: {v}");
		assert_eq!(v["Options"]["type"], "nfs");
	}
}

/// The `system/df` response, reduced to the part `volumes --size` needs.
///
/// **This is the only endpoint that knows a volume's size.** The volume list
/// carries no size field at all, so answering "how big is this volume" means
/// asking libpod to account for every image, container and volume it owns —
/// measured at 1.2 s against 10 ms for the plain list on a host with 46
/// volumes. That cost is why the column is opt-in rather than default.
#[derive(serde::Deserialize, Default)]
pub struct SystemDf {
	/// Per-volume disk accounting.
	#[serde(rename = "Volumes", default)]
	pub volumes: Vec<VolumeDiskUsage>,
}

/// One volume's disk usage, as `system/df` reports it.
#[derive(serde::Deserialize, Clone)]
pub struct VolumeDiskUsage {
	/// The on-host volume name, which is what the list endpoint calls `Name`.
	#[serde(rename = "VolumeName", default)]
	pub name: String,
	/// Bytes the volume occupies.
	#[serde(rename = "Size", default)]
	pub size: u64,
	/// Bytes that would be freed by removing it — zero while a container still
	/// links the volume, which is what makes it worth showing next to the size
	/// rather than instead of it.
	#[serde(rename = "ReclaimableSize", default)]
	pub reclaimable: u64,
	/// How many containers reference the volume.
	#[serde(rename = "Links", default)]
	pub links: u64,
}
