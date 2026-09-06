//! Short-form volume-spec string parsing.
//!
//! Splits `"src:dst[:opts]"` strings (and pre-built secret/config bind strings)
//! into OCI `Mount` / `NamedVolume` parts, handling the Windows drive-letter
//! colon so it is not mistaken for the `src:dst` separator.

use crate::compose::types::{BindOptions, VolumeOptions};
use crate::libpod::types::container::{Mount, NamedVolume};

/// Parse a short-form volume string `"src:dst"` or `"src:dst:opts"`.
///
/// Returns `Some((mount, named))` where exactly one of the two is `Some`.
/// Named volumes go to `SpecGenerator.volumes`; bind mounts go to `mounts`.
pub(super) fn parse_volume_string(s: &str) -> Option<(Option<Mount>, Option<NamedVolume>)> {
	let (src, dst, opts_str) = split_volume_spec(s);
	let opts: Vec<String> = opts_str
		.split(',')
		.map(|o| o.trim().to_string())
		.filter(|o| !o.is_empty())
		.collect();
	// A colon-less entry (`/data`, `cache`) is a single in-container target, not
	// a `src:dst` pair: per the compose-spec it provisions an anonymous volume at
	// that path. Without this, `is_bind_source("/data")` is true and the leading
	// slash would be mistaken for a host bind, mounting the host's `/data`.
	if !spec_has_separator(s) {
		return Some((
			None,
			Some(NamedVolume {
				name: String::new(),
				dest: dst.to_string(),
				options: opts,
				sub_path: None,
			}),
		));
	}
	if is_bind_source(src) {
		Some((
			Some(Mount {
				mount_type: "bind".into(),
				source: if src.is_empty() {
					None
				} else {
					Some(src.to_string())
				},
				destination: dst.to_string(),
				options: opts,
			}),
			None,
		))
	} else {
		Some((
			None,
			Some(NamedVolume {
				name: src.to_string(),
				dest: dst.to_string(),
				options: opts,
				sub_path: None,
			}),
		))
	}
}

/// Whether `s` begins with a Windows drive-letter prefix (e.g. `C:`), so the
/// colon that follows is part of the path rather than a `src:dst` separator.
fn has_windows_drive_prefix(s: &str) -> bool {
	let b = s.as_bytes();
	b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Classify a short-form volume source. A leading `/`, `.` or `~` marks a host
/// path bind; a Windows drive prefix (`C:\...`) does too. Anything else is a
/// named volume.
fn is_bind_source(src: &str) -> bool {
	src.starts_with('/')
		|| src.starts_with('.')
		|| src.starts_with('~')
		|| has_windows_drive_prefix(src)
}

/// Whether a short-form spec carries a `src:dst` separator, i.e. at least one
/// colon outside a leading Windows drive prefix. A spec with none is a single
/// in-container path (anonymous volume) rather than a `src:dst` pair.
fn spec_has_separator(s: &str) -> bool {
	let scan_from = if has_windows_drive_prefix(s) { 2 } else { 0 };
	s.bytes().skip(scan_from).any(|b| b == b':')
}

/// Split a short-form volume spec into `(src, dst, opts)`. Colons separate the
/// fields, except the colon in a leading Windows drive prefix, which belongs to
/// the source path. The destination is always an in-container (Unix) path, so
/// only the source can carry a drive letter.
fn split_volume_spec(s: &str) -> (&str, &str, &str) {
	let scan_from = if has_windows_drive_prefix(s) { 2 } else { 0 };
	let seps: Vec<usize> = s
		.bytes()
		.enumerate()
		.skip(scan_from)
		.filter(|&(_, b)| b == b':')
		.map(|(i, _)| i)
		.take(2)
		.collect();
	match seps.as_slice() {
		[] => (s, s, ""),
		[a] => (&s[..*a], &s[a + 1..], ""),
		[a, b] => (&s[..*a], &s[a + 1..*b], &s[b + 1..]),
		_ => unreachable!("take(2) yields at most two separators"),
	}
}

/// Parse one entry of the short `tmpfs:` list into a tmpfs Mount.
///
/// The entry is `<path>` or `<path>:<opt>[,<opt>…]`, split on the **first**
/// colon: everything after it is handed to the engine as mount options. That is
/// what docker compose does, measured against it rather than inferred, for
/// `/multi:size=64m,mode=1777,noexec` it produces the Tmpfs entry
/// `/multi -> size=64m,mode=1777,noexec,…`, and a bare `/plain` or a trailing
/// `/trail:` yields the path with no options.
///
/// Options are passed through verbatim, not trimmed or validated: docker compose
/// does not sanitise them either, and letting the engine reject `unknown mount
/// option " noexec"` is a better answer than silently repairing a typo.
pub(super) fn parse_tmpfs_string(s: &str) -> Mount {
	let (dst, opts_str) = s.split_once(':').unwrap_or((s, ""));
	Mount {
		mount_type: "tmpfs".into(),
		source: None,
		destination: dst.to_string(),
		options: opts_str
			.split(',')
			.filter(|o| !o.is_empty())
			.map(str::to_string)
			.collect(),
	}
}

pub(super) fn access_opts(read_only: Option<bool>) -> Vec<String> {
	if read_only.unwrap_or(false) {
		vec!["ro".into()]
	} else {
		vec!["rw".into()]
	}
}

pub(super) fn extend_bind_opts_str(opts: &mut Vec<String>, b: Option<&BindOptions>) {
	let Some(b) = b else { return };
	if let Some(p) = &b.propagation {
		opts.push(p.clone());
	}
	if let Some(s) = &b.selinux {
		opts.push(map_selinux_option(s));
	}
}

/// Translate a Compose `bind.selinux` value into the Podman mount option.
///
/// Compose spells the SELinux relabel mode as `shared`/`private`; Podman's mount
/// options are `z` (shared label, usable by multiple containers) and `Z` (private
/// label, scoped to one container). Map those two words; pass anything else
/// through verbatim so a caller can still supply a raw `z`/`Z` directly.
fn map_selinux_option(value: &str) -> String {
	match value {
		"shared" => "z".to_string(),
		"private" => "Z".to_string(),
		other => other.to_string(),
	}
}

pub(super) fn extend_volume_opts_str(opts: &mut Vec<String>, v: Option<&VolumeOptions>) {
	let Some(v) = v else { return };
	if v.nocopy.unwrap_or(false) {
		opts.push("nocopy".into());
	}
	// The mount-hardening trio. The short form has always carried these as raw
	// options; the long form dropped them with an unknown-key warning, so the
	// spelled-out mount was the one that could not be hardened (#1160).
	if v.noexec.unwrap_or(false) {
		opts.push("noexec".into());
	}
	if v.nosuid.unwrap_or(false) {
		opts.push("nosuid".into());
	}
	if v.nodev.unwrap_or(false) {
		opts.push("nodev".into());
	}
}

#[cfg(test)]
#[path = "spec_tests.rs"]
mod tests;
