//! Container-spec field builders: device mappings, block-I/O throttling,
//! label-file labels, and Swarm-only deploy-field warnings.

// libc FFI (stat, for device major/minor) is needed here; each block carries a
// soundness comment.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;

use tracing::warn;

use crate::compose::types::{BlkioConfig, Service};
use crate::error::ComposeError;
use crate::libpod::types::container::{
	LinuxBlockIO, LinuxDevice, LinuxDeviceCgroup, LinuxThrottleDevice, LinuxWeightDevice,
};

// ---------------------------------------------------------------------------
// Device helpers
// ---------------------------------------------------------------------------

/// A parsed compose `devices:` entry: the device node to create plus, when the
/// entry carried an explicit `:permissions` segment, the cgroup access rule that
/// restricts it.
pub(crate) struct ParsedDevice {
	/// The device node to expose inside the container.
	pub device: LinuxDevice,
	/// The cgroup access rule derived from the trailing `:permissions` segment,
	/// present only when one was given.
	pub cgroup_rule: Option<LinuxDeviceCgroup>,
}

/// Parse a compose `devices:` entry (`host:container:permissions`) into a
/// [`ParsedDevice`]. The container path defaults to the host path when the
/// `:container` segment is absent; major/minor/type are derived by `stat`ing the
/// host node. A trailing `:permissions` segment (e.g. `r`, `rwm`) is retained as
/// a `device_cgroup_rule`: the OCI `LinuxDevice` has no access field, so the
/// restriction must ride alongside as a cgroup rule for the live up path to
/// honor it consistently with the quadlet backend and docker-compose.
pub(crate) fn parse_device(s: &str) -> ParsedDevice {
	let parts: Vec<&str> = s.splitn(3, ':').collect();
	let host = parts.first().copied().unwrap_or("").to_string();
	let cont = parts
		.get(1)
		.copied()
		.map(|c| c.to_string())
		.unwrap_or_else(|| host.clone());
	let access = parts
		.get(2)
		.copied()
		.filter(|p| !p.is_empty())
		.map(str::to_string);

	let (major, minor, device_type) = device_major_minor(&host);

	let cgroup_rule = access.map(|access| LinuxDeviceCgroup {
		allow: true,
		device_type: Some(device_type.clone()),
		major: Some(major),
		minor: Some(minor),
		access: Some(access),
	});

	ParsedDevice {
		device: LinuxDevice {
			path: cont,
			device_type,
			major,
			minor,
			file_mode: None,
			uid: None,
			gid: None,
		},
		cgroup_rule,
	}
}

/// Linux device number encoding uses 64-bit `dev_t`; the formula is Linux-kernel specific.
#[cfg(target_os = "linux")]
fn device_major_minor(path: &str) -> (i64, i64, String) {
	use std::ffi::CString;
	let Ok(c_path) = CString::new(path) else {
		return (0, 0, "c".to_string());
	};
	// SAFETY: `libc::stat` is a plain C struct of integers; an all-zero bit
	// pattern is a valid initial value that `libc::stat()` fully overwrites.
	let mut st: libc::stat = unsafe { std::mem::zeroed() };
	// SAFETY: `c_path` is a valid NUL-terminated C string that outlives the
	// call, and `&mut st` points to a live, correctly-sized `stat`. The return
	// value is checked before any field of `st` is read.
	if unsafe { libc::stat(c_path.as_ptr(), &mut st) } != 0 {
		return (0, 0, "c".to_string());
	}
	let rdev = st.st_rdev as u64;
	let major = (((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff)) as i64;
	let minor = ((rdev & 0xff) | ((rdev >> 12) & !0xff)) as i64;
	let dev_type = if st.st_mode & libc::S_IFMT == libc::S_IFBLK {
		"b"
	} else {
		"c"
	};
	(major, minor, dev_type.to_string())
}

/// Non-Linux Unix (macOS): Podman runs via a VM; host device paths don't translate to Linux device numbers.
#[cfg(all(unix, not(target_os = "linux")))]
fn device_major_minor(_path: &str) -> (i64, i64, String) {
	(0, 0, "c".to_string())
}

#[cfg(not(unix))]
fn device_major_minor(_path: &str) -> (i64, i64, String) {
	(0, 0, "c".to_string())
}

// ---------------------------------------------------------------------------
// Blkio
// ---------------------------------------------------------------------------

pub(super) fn build_blkio_config(service: &Service) -> Option<LinuxBlockIO> {
	let cfg: &BlkioConfig = service.blkio_config.as_ref()?;

	let weight_device = cfg
		.weight_device
		.iter()
		.map(|d| {
			let (major, minor, _) = device_major_minor(&d.path);
			LinuxWeightDevice {
				major,
				minor,
				weight: Some(d.weight),
			}
		})
		.collect();

	let throttle = |devs: &[crate::compose::types::BlkioRateDevice]| -> Vec<LinuxThrottleDevice> {
		devs.iter()
			.map(|d| {
				let (major, minor, _) = device_major_minor(&d.path);
				LinuxThrottleDevice {
					major,
					minor,
					rate: d.rate_value() as u64,
				}
			})
			.collect()
	};

	Some(LinuxBlockIO {
		weight: cfg.weight,
		weight_device,
		throttle_read_bps_device: throttle(&cfg.device_read_bps),
		throttle_write_bps_device: throttle(&cfg.device_write_bps),
		throttle_read_iops_device: throttle(&cfg.device_read_iops),
		throttle_write_iops_device: throttle(&cfg.device_write_iops),
	})
}

// ---------------------------------------------------------------------------
// Label helpers
// ---------------------------------------------------------------------------

/// Maximum byte length of a label key, matching podman's per-label cap. A label
/// file with an oversize key would either be truncated by libpod or rejected at
/// the libpod layer with an opaque 400; rejecting up front gives a clearer
/// message and closes the silent-truncation path.
pub(super) const MAX_LABEL_KEY_LEN: usize = 253;
/// Maximum byte length of a label value (4 KiB). Podman's own cap is higher,
/// but a hostile `label_file:` value would otherwise be JSON-accepted by
/// libpod and then handed to downstream consumers that re-parse the value,
/// where a multi-megabyte string is at best useless and at worst a DoS vector.
/// 4 KiB is generous for any real label and bounds the worst case.
pub(super) const MAX_LABEL_VALUE_LEN: usize = 4 * 1024;
/// Maximum number of distinct label entries podup will accept from a single
/// `label_file:` pass. The 16 MiB read cap on the file does not constrain the
/// resulting HashMap size, so a 16 MiB file of single-character keys could
/// otherwise produce a 16M-entry map. 64 is well past any real label set and
/// bounds the worst case.
pub(super) const MAX_LABEL_FILE_ENTRIES: usize = 64;

/// Why [`sanitize_kv_pair`] rejected a `label_file:` entry. Returned rather
/// than a `bool` so the caller can produce a specific error message that names
/// the offending axis (key vs value vs cap) without re-checking the rules.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum SanitizeError {
	/// The key is empty, contains an ASCII control character, or exceeds
	/// [`MAX_LABEL_KEY_LEN`] bytes.
	InvalidKey,
	/// The value contains an ASCII control character or exceeds
	/// [`MAX_LABEL_VALUE_LEN`] bytes.
	InvalidValue,
	/// The map is already at [`MAX_LABEL_FILE_ENTRIES`] distinct keys and this
	/// entry would add a new one (an overwrite of an existing key is allowed).
	TooManyEntries,
}

/// Sanitize a single key/value pair parsed from a `label_file:` line and insert
/// it into `labels`, enforcing:
///
/// * per-pair constraints on the key: non-empty, no ASCII control characters,
///   length ≤ [`MAX_LABEL_KEY_LEN`];
/// * per-pair constraints on the value: no ASCII control characters, length ≤
///   [`MAX_LABEL_VALUE_LEN`];
/// * the per-file entry cap [`MAX_LABEL_FILE_ENTRIES`] on distinct keys.
///
/// Podman JSON-encodes the value on the wire, so wire injection is not the
/// concern; the rejection is for **downstream consumers that re-parse the label
/// value** (logs, `ls`/`inspect` UIs, label-based filters), where a control
/// character in either side lets a single entry break out of its context.
///
/// Returns the inserted `(key, value)` on success, or the specific
/// [`SanitizeError`] variant on rejection. On rejection `labels` is left
/// unchanged so the caller can decide what to surface.
pub(super) fn sanitize_kv_pair(
	labels: &mut HashMap<String, String>,
	key: &str,
	value: &str,
) -> Result<(String, String), SanitizeError> {
	if key.is_empty() || key.len() > MAX_LABEL_KEY_LEN || key.chars().any(|c| c.is_control()) {
		return Err(SanitizeError::InvalidKey);
	}
	if value.len() > MAX_LABEL_VALUE_LEN || value.chars().any(|c| c.is_control()) {
		return Err(SanitizeError::InvalidValue);
	}
	// Overwrite of an existing key is allowed even at the cap: the cap bounds
	// the number of distinct keys, not total insertions.
	if !labels.contains_key(key) && labels.len() >= MAX_LABEL_FILE_ENTRIES {
		return Err(SanitizeError::TooManyEntries);
	}
	let k = key.to_string();
	let v = value.to_string();
	labels.insert(k.clone(), v.clone());
	Ok((k, v))
}

/// Encode a compose-file path string for inclusion in the comma-joined
/// `podup.config-files` label. A `,` in a path would visually merge with the
/// next entry when the label is split back on `,`; `%2C` round-trips through
/// that split unambiguously. Newlines are not produced by `Path::display` on
/// any supported platform and are left as-is; the libpod JSON encoding would
/// re-escape them anyway. The underlying `PathBuf` is unaffected: this is a
/// render-side concern only.
pub(super) fn encode_path_for_label(path_str: &str) -> String {
	if path_str.contains(',') {
		path_str.replace(',', "%2C")
	} else {
		path_str.to_string()
	}
}

pub(super) fn build_label_file_labels(
	service: &Service,
	base_dir: &Path,
) -> Result<HashMap<String, String>, ComposeError> {
	let mut labels = HashMap::new();
	for path in service.label_file.to_list() {
		let full = if std::path::Path::new(&path).is_absolute() {
			std::path::PathBuf::from(&path)
		} else {
			base_dir.join(&path)
		};
		let content = match crate::filesystem::read_to_string_capped(&full) {
			Ok(c) => c,
			Err(e) => {
				warn!("label_file: cannot read {}: {e}", full.display());
				continue;
			}
		};
		for (line_idx, line) in content.lines().enumerate() {
			let trimmed = line.trim();
			if trimmed.is_empty() || trimmed.starts_with('#') {
				continue;
			}
			let mut parts = trimmed.splitn(2, '=');
			let key = parts.next().unwrap_or("").trim();
			let val = parts.next().unwrap_or("");
			if key.is_empty() {
				continue;
			}
			if let Err(why) = sanitize_kv_pair(&mut labels, key, val) {
				return Err(ComposeError::Unsupported(format!(
					"label_file {} line {}: rejected ({why:?})",
					full.display(),
					line_idx + 1,
				)));
			}
		}
	}
	Ok(labels)
}

/// Resolve the user-facing labels for a container.
///
/// Merges `service.labels` with any labels sourced from `label_file`, with
/// `service.labels` taking precedence. Per the Compose Specification,
/// `deploy.labels` are set on the service only and are deliberately NOT applied
/// to containers, matching docker-compose v2 behaviour.
pub(super) fn resolve_container_labels(
	service: &Service,
	label_file_labels: HashMap<String, String>,
) -> HashMap<String, String> {
	let mut labels = service.labels.to_map();
	for (k, v) in label_file_labels {
		labels.entry(k).or_insert(v);
	}
	labels
}

// ---------------------------------------------------------------------------
// Swarm-only deploy field diagnostics
// ---------------------------------------------------------------------------

pub(super) fn warn_swarm_only_deploy(service_name: &str, service: &Service) {
	let Some(deploy) = &service.deploy else {
		return;
	};

	if let Some(mode) = &deploy.mode {
		warn!(
			"service \"{service_name}\": deploy.mode=\"{mode}\" is a Docker Swarm field \
			and has no effect on single-host Podman"
		);
	}
	if deploy.placement.is_some() {
		warn!(
			"service \"{service_name}\": deploy.placement is a Docker Swarm field \
			and has no effect on single-host Podman"
		);
	}
	if deploy.update_config.is_some() {
		warn!(
			"service \"{service_name}\": deploy.update_config is a Docker Swarm field \
			and has no effect on single-host Podman"
		);
	}
	if deploy.rollback_config.is_some() {
		warn!(
			"service \"{service_name}\": deploy.rollback_config is a Docker Swarm field \
			and has no effect on single-host Podman"
		);
	}
	if let Some(mode) = &deploy.endpoint_mode {
		warn!(
			"service \"{service_name}\": deploy.endpoint_mode=\"{mode}\" is a Docker Swarm field \
			and has no effect on single-host Podman"
		);
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
