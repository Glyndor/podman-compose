//! Unknown-key warnings for nested compose *option blocks*.
//!
//! The typed [`ComposeFile`](crate::compose::types::ComposeFile) silently drops
//! any key it does not model inside seven nested option blocks: a `bind`,
//! `volume`, or `tmpfs` mount block, a long-form service `networks.<net>` map,
//! and the `deploy.resources.{limits,reservations}` specs (plus their
//! `driver_config` / `devices[]` children). Unlike the service- and top-level
//! passes, these structs carry no `#[serde(flatten)]` unknown bucket (adding one
//! would change a public type and break the 1.x SemVer gate), so the dropped
//! keys are unreachable from the parsed model.
//!
//! This pass therefore works from the *raw, interpolated* YAML document instead.
//! For every such block it compares the present keys against an explicit
//! allowlist of the keys that block's type models: a key that is neither modeled
//! nor an `x-` extension is reported.
//!
//! The allowlist is deliberate, not derived from a round-trip. A round-trip
//! (serialize-the-parsed-struct) would drop any modeled key whose value is
//! `None`/empty (every field carries `skip_serializing_if`), so `propagation:`
//! null, `link_local_ips: []`, `driver_opts: {}`, or `devices: []` would be
//! mis-flagged as unknown, the forbidden "warn on a modeled key" case. A guard
//! test per type (see `tests`) serializes a fully-populated, exhaustive struct
//! literal and asserts its key set equals the allowlist, so adding a field to
//! any of the seven structs fails to compile until both the literal and the
//! allowlist are updated.

// --- Per-type allowlists of modeled serde keys -----------------------------
//
// Each entry is the YAML key serde reads/writes for the field (accounting for
// any `#[serde(rename)]`; none of the seven currently rename). Kept in sync with
// the structs by the exhaustive-literal guard tests below.

/// `BindOptions` (volume.rs).
const BIND_OPTIONS_KEYS: &[&str] = &["propagation", "create_host_path", "selinux"];
/// `VolumeOptions` (volume.rs).
const VOLUME_OPTIONS_KEYS: &[&str] = &[
	"nocopy",
	"labels",
	"driver_config",
	"subpath",
	"noexec",
	"nosuid",
	"nodev",
];
/// `DriverConfig` (volume.rs).
const DRIVER_CONFIG_KEYS: &[&str] = &["name", "options"];
/// `TmpfsOptions` (volume.rs).
const TMPFS_OPTIONS_KEYS: &[&str] = &["size", "mode"];
/// `ServiceNetworkConfig` (network.rs).
const SERVICE_NETWORK_CONFIG_KEYS: &[&str] = &[
	"aliases",
	"ipv4_address",
	"ipv6_address",
	"link_local_ips",
	"priority",
	"mac_address",
	"driver_opts",
	"gw_priority",
	"interface_name",
];
/// `ResourceSpec` (deploy.rs).
const RESOURCE_SPEC_KEYS: &[&str] = &["cpus", "memory", "pids", "devices"];
/// `DeviceReservation` (deploy.rs).
const DEVICE_RESERVATION_KEYS: &[&str] =
	&["capabilities", "count", "device_ids", "driver", "options"];

/// Collect unknown-key warnings for every nested option block in an already
/// interpolated, merge-resolved compose document.
///
/// Pure (no I/O, no logging) so it is unit-testable; the caller emits each
/// message via `tracing::warn!`. An unparseable document yields no warnings; it
/// is the parser proper's job to report that.
pub(crate) fn raw_nested_unknown_warnings(interpolated_yaml: &str) -> Vec<String> {
	let mut out = Vec::new();
	let Ok(doc) = serde_yaml::from_str::<serde_yaml::Value>(interpolated_yaml) else {
		return out;
	};
	let Some(services) = doc.get("services").and_then(|v| v.as_mapping()) else {
		return out;
	};
	for (name, def) in services {
		let (Some(service), Some(svc)) = (name.as_str(), def.as_mapping()) else {
			continue;
		};
		walk_volumes(service, svc, &mut out);
		walk_networks(service, svc, &mut out);
		walk_deploy(service, svc, &mut out);
	}
	out
}

/// `services.<svc>.volumes[i].{bind,volume,tmpfs}` (long-form mounts only).
fn walk_volumes(service: &str, svc: &serde_yaml::Mapping, out: &mut Vec<String>) {
	let Some(mounts) = svc.get("volumes").and_then(|v| v.as_sequence()) else {
		return;
	};
	for (i, mount) in mounts.iter().enumerate() {
		// Short-form `"src:dst"` string mounts have no option block.
		let Some(m) = mount.as_mapping() else {
			continue;
		};
		if let Some(bind) = m.get("bind").and_then(|v| v.as_mapping()) {
			diff_unknown(
				bind,
				BIND_OPTIONS_KEYS,
				&format!("service '{service}' volumes[{i}].bind"),
				out,
			);
		}
		if let Some(volume) = m.get("volume").and_then(|v| v.as_mapping()) {
			diff_unknown(
				volume,
				VOLUME_OPTIONS_KEYS,
				&format!("service '{service}' volumes[{i}].volume"),
				out,
			);
			// `driver_config` is itself an option block; the parent allowlist only
			// records its presence, so recurse to reach its own unknown keys.
			if let Some(dc) = volume.get("driver_config").and_then(|v| v.as_mapping()) {
				diff_unknown(
					dc,
					DRIVER_CONFIG_KEYS,
					&format!("service '{service}' volumes[{i}].volume.driver_config"),
					out,
				);
			}
		}
		if let Some(tmpfs) = m.get("tmpfs").and_then(|v| v.as_mapping()) {
			diff_unknown(
				tmpfs,
				TMPFS_OPTIONS_KEYS,
				&format!("service '{service}' volumes[{i}].tmpfs"),
				out,
			);
		}
	}
}

/// `services.<svc>.networks.<net>`: only the long-form mapping carries options;
/// a bare list or a `null` attachment has nothing to diff.
fn walk_networks(service: &str, svc: &serde_yaml::Mapping, out: &mut Vec<String>) {
	let Some(nets) = svc.get("networks").and_then(|v| v.as_mapping()) else {
		return;
	};
	for (net, cfg) in nets {
		let (Some(net), Some(cfg)) = (net.as_str(), cfg.as_mapping()) else {
			continue;
		};
		diff_unknown(
			cfg,
			SERVICE_NETWORK_CONFIG_KEYS,
			&format!("service '{service}' networks.{net}"),
			out,
		);
	}
}

/// `services.<svc>.deploy.resources.{limits,reservations}` and their
/// `devices[]` children.
fn walk_deploy(service: &str, svc: &serde_yaml::Mapping, out: &mut Vec<String>) {
	let Some(resources) = svc
		.get("deploy")
		.and_then(|v| v.as_mapping())
		.and_then(|d| d.get("resources"))
		.and_then(|r| r.as_mapping())
	else {
		return;
	};
	for kind in ["limits", "reservations"] {
		let Some(spec) = resources.get(kind).and_then(|v| v.as_mapping()) else {
			continue;
		};
		diff_unknown(
			spec,
			RESOURCE_SPEC_KEYS,
			&format!("service '{service}' deploy.resources.{kind}"),
			out,
		);
		let Some(devices) = spec.get("devices").and_then(|v| v.as_sequence()) else {
			continue;
		};
		for (j, dev) in devices.iter().enumerate() {
			if let Some(d) = dev.as_mapping() {
				diff_unknown(
					d,
					DEVICE_RESERVATION_KEYS,
					&format!("service '{service}' deploy.resources.{kind}.devices[{j}]"),
					out,
				);
			}
		}
	}
}

/// Report every key in `m` that is neither in `known` (the type's modeled serde
/// keys) nor an `x-` extension. No deserialization is involved: comparing
/// against the explicit allowlist means a modeled key is never flagged, even
/// when its value is null/empty and would have been dropped by a round-trip.
fn diff_unknown(m: &serde_yaml::Mapping, known: &[&str], context: &str, out: &mut Vec<String>) {
	for key in m.keys() {
		let Some(key) = key.as_str() else {
			continue;
		};
		if key.starts_with("x-") || known.contains(&key) {
			continue;
		}
		out.push(format!(
			"{context}: unknown key '{key}' is ignored \
			 (check for a typo or an unsupported compose feature)"
		));
	}
}

#[cfg(test)]
#[path = "nested_raw_tests.rs"]
mod tests;
