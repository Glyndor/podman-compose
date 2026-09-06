//! Volume mount helpers.
//!
//! [`build_mounts_all`] converts all `volumes:` entries into OCI `Mount` entries
//! and `NamedVolume` entries for the SpecGenerator. Named volumes go in
//! `volumes`; everything else (bind, tmpfs, npipe, cluster) goes in `mounts`.
//! Secrets and configs are not here: every source is a Podman-native secret
//! attached to the spec, never a mount.

use std::path::Path;

use crate::compose::types::{Service, VolumeMount, VolumeType};
use crate::libpod::types::container::{Mount, NamedVolume};

mod spec;
use spec::{access_opts, extend_bind_opts_str, extend_volume_opts_str, parse_volume_string};

/// Build all OCI mounts and named volume attachments for a container.
///
/// Returns `(mounts, named_volumes)`. Named volumes must go into
/// `SpecGenerator.volumes`; bind/tmpfs/npipe mounts go into
/// `SpecGenerator.mounts`.
pub(crate) fn build_mounts_all(
	service: &Service,
	base_dir: &Path,
) -> (Vec<Mount>, Vec<NamedVolume>) {
	let mut mounts = Vec::new();
	let mut named = Vec::new();

	for v in &service.volumes {
		match v {
			VolumeMount::Short(s) => {
				if let Some((m, n)) = parse_volume_string(s) {
					match n {
						Some(nv) => named.push(nv),
						None => {
							let m = m.unwrap();
							// Short-form binds imply `create_host_path` (compose-spec),
							// so create a missing host source directory before mounting.
							// Otherwise `up` aborts with a raw podman HTTP 500 statfs
							// error that leaks the absolute host path. Resolve exactly
							// like the mount source so the directory is created at the
							// path actually bind-mounted (relative anchored to the
							// project dir, leading `~` expanded).
							if let Some(src) = m.source.as_deref() {
								let abs = super::container::resolve_bind_source(src, base_dir);
								if let Err(e) = std::fs::create_dir_all(&abs) {
									tracing::warn!("create_host_path: failed to create {abs}: {e}");
								}
							}
							mounts.push(m);
						}
					}
				}
			}
			VolumeMount::Long {
				volume_type,
				source,
				target,
				read_only,
				bind,
				volume,
				tmpfs,
				..
			} => match volume_type {
				VolumeType::Tmpfs => {
					let mut opts: Vec<String> = Vec::new();
					if let Some(t) = tmpfs {
						if let Some(size) = t.size {
							opts.push(format!("size={size}"));
						}
						if let Some(mode) = t.mode {
							opts.push(format!("mode={mode:o}"));
						}
					}
					if read_only.unwrap_or(false) {
						opts.push("ro".into());
					}
					mounts.push(Mount {
						mount_type: "tmpfs".into(),
						source: None,
						destination: target.clone(),
						options: opts,
					});
				}
				VolumeType::Bind => {
					let src = source.as_deref().unwrap_or("");

					if let Some(b) = bind {
						if b.create_host_path.unwrap_or(false) && !src.is_empty() {
							// Resolve exactly like the mount source (expand `~`, anchor a
							// relative path to the project dir) so the directory is created
							// at the path actually bind-mounted, not a literal `~` dir.
							let abs = super::container::resolve_bind_source(src, base_dir);
							if let Err(e) = std::fs::create_dir_all(&abs) {
								tracing::warn!("create_host_path: failed to create {abs}: {e}");
							}
						}
					}

					let mut opts = access_opts(*read_only);
					extend_bind_opts_str(&mut opts, bind.as_ref());
					mounts.push(Mount {
						mount_type: "bind".into(),
						source: Some(src.to_string()),
						destination: target.clone(),
						options: opts,
					});
				}
				VolumeType::Volume => {
					let mut opts = access_opts(*read_only);
					extend_volume_opts_str(&mut opts, volume.as_ref());
					named.push(NamedVolume {
						name: source.clone().unwrap_or_default(),
						dest: target.clone(),
						options: opts,
						sub_path: volume.as_ref().and_then(|v| v.subpath.clone()),
					});
				}
				VolumeType::Npipe => {
					mounts.push(Mount {
						mount_type: "npipe".into(),
						source: source.clone(),
						destination: target.clone(),
						options: vec![],
					});
				}
				VolumeType::Cluster => {
					mounts.push(Mount {
						mount_type: "cluster".into(),
						source: source.clone(),
						destination: target.clone(),
						options: vec![],
					});
				}
			},
		}
	}

	// Top-level `tmpfs:` shorthand, equivalent to volumes with type=tmpfs.
	for entry in service.tmpfs.to_list() {
		mounts.push(spec::parse_tmpfs_string(&entry));
	}

	(mounts, named)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
