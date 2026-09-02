//! Security/device builders that translate compose fields onto the dedicated
//! Podman SpecGenerator fields: `security_opt` decomposition, `device_cgroup_rules`
//! parsing, and CDI device injection. Podman 5.x has no `security_opt` or
//! `cdi_devices` SpecGenerator field, and `device_cgroup_rule` is structured, not
//! a string list — sending the compose shapes verbatim silently drops them (or,
//! for cgroup rules, fails the request), so each is converted here.

use tracing::warn;

use crate::compose::types::Service;
use crate::libpod::types::container::{LinuxDevice, LinuxDeviceCgroup};

/// Build a `LinuxDevice` carrying a CDI qualified name (e.g. `nvidia.com/gpu=all`).
///
/// Podman 5.x has no SpecGenerator CDI field: `ExtractCDIDevices` scans the
/// `devices` array and pulls out any entry whose `path` is a CDI qualified name,
/// ignoring the major/minor/type fields entirely. So the name rides in as the
/// device path with the device-node fields left zeroed.
pub(crate) fn cdi_device(name: String) -> LinuxDevice {
	LinuxDevice {
		path: name,
		device_type: String::new(),
		major: 0,
		minor: 0,
		file_mode: None,
		uid: None,
		gid: None,
	}
}

/// Decomposed `security_opt:` values, matching Podman's SpecGenerator security
/// fields (it has no single `security_opt` field — see [`parse_security_opts`]).
#[derive(Default)]
pub(crate) struct SecurityOptions {
	pub selinux_opts: Vec<String>,
	pub apparmor_profile: Option<String>,
	pub seccomp_profile_path: Option<String>,
	pub no_new_privileges: Option<bool>,
	pub mask: Vec<String>,
	pub unmask: Vec<String>,
}

/// Split each compose `security_opt:` entry onto the dedicated SpecGenerator
/// field Podman expects. Compose accepts both `:` and `=` separators; the value
/// is taken after the first one. Mirrors Podman's own `--security-opt` parser:
/// `mask` is colon-split, the rest take the value whole.
pub(crate) fn parse_security_opts(service: &Service) -> SecurityOptions {
	let mut out = SecurityOptions::default();
	for opt in &service.security_opt {
		let (key, val) = match opt.find([':', '=']) {
			Some(i) => (&opt[..i], &opt[i + 1..]),
			None => (opt.as_str(), ""),
		};
		match key {
			"no-new-privileges" => {
				out.no_new_privileges = Some(val.is_empty() || val.eq_ignore_ascii_case("true"));
			}
			"label" => out.selinux_opts.push(val.to_string()),
			"apparmor" => out.apparmor_profile = Some(val.to_string()),
			"seccomp" => out.seccomp_profile_path = Some(val.to_string()),
			"mask" => out.mask.extend(val.split(':').map(str::to_string)),
			"unmask" => out.unmask.push(val.to_string()),
			other => warn!("security_opt '{other}' has no SpecGenerator field and is ignored"),
		}
	}
	out
}

/// Parse one `device_cgroup_rules:` entry (`"<type> <major>:<minor> <access>"`,
/// e.g. `c 1:3 rwm` or `a *:* rwm`) into the structured `LinuxDeviceCgroup`
/// Podman expects. `*` means "all" (a `None` major/minor). Returns `None` on a
/// malformed rule so the caller can warn and skip it rather than send a body
/// Podman rejects.
pub(crate) fn parse_device_cgroup_rule(rule: &str) -> Option<LinuxDeviceCgroup> {
	let mut fields = rule.split_whitespace();
	let device_type = fields.next()?;
	let major_minor = fields.next()?;
	let access = fields.next()?;
	if fields.next().is_some() {
		return None;
	}
	let (major, minor) = major_minor.split_once(':')?;
	let num = |s: &str| -> Option<Option<i64>> {
		if s == "*" {
			Some(None)
		} else {
			s.parse::<i64>().ok().map(Some)
		}
	};
	Some(LinuxDeviceCgroup {
		allow: true,
		device_type: Some(device_type.to_string()),
		major: num(major)?,
		minor: num(minor)?,
		access: Some(access.to_string()),
	})
}

#[cfg(test)]
#[path = "security_tests.rs"]
mod tests;
