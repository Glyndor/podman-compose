//! Render service `secrets:` and `security_opt:` onto their Quadlet keys.

use indexmap::IndexMap;

use crate::compose::types::{SecretConfig, ServiceSecretRef};

use super::Section;

/// Sanitize one `Secret=` option-list field: drop control characters and the
/// `,`/`=` separators so a hostile compose value cannot inject extra options.
pub(super) fn secret_field(value: &str) -> String {
	value
		.chars()
		.filter(|c| !c.is_control() && *c != ',' && *c != '=')
		.collect()
}

/// Whether a top-level secret definition is an inline `content:`/`environment:`
/// source, the kind `up` materialises as a project-scoped native Podman secret.
/// `external:` wins (never created by podup) and a bare `file:`/empty def is a
/// bind/host source kept under its compose name.
pub(super) fn is_inline_secret(def: Option<&SecretConfig>) -> bool {
	def.is_some_and(|d| {
		d.external != Some(true) && (d.content.is_some() || d.environment.is_some())
	})
}

/// Resolve the `Secret=` source name for a service secret reference. An inline
/// secret resolves to the project-scoped name `{project}_secret_{name}` that
/// `up` creates (via `plan::scoped_name`), so generated units reference the same
/// secret `up` would; any other secret keeps its compose source name.
fn secret_source_name(
	project: &str,
	source: &str,
	secrets: &IndexMap<String, SecretConfig>,
) -> String {
	if is_inline_secret(secrets.get(source)) {
		format!("{project}_secret_{source}")
	} else {
		source.to_string()
	}
}

/// Render a service `secrets:` entry into a Quadlet `Secret=` value
/// (`name[,target=,uid=,gid=,mode=]`), resolving inline secrets to their
/// project-scoped name so the reference matches what `up` creates.
pub(super) fn render_secret(
	secret: &ServiceSecretRef,
	project: &str,
	secrets: &IndexMap<String, SecretConfig>,
) -> String {
	match secret {
		// Sanitize the short-form name too: `Secret=` is an option list, so a
		// `,`/`=` in the name would inject extra options (same guard as Long).
		ServiceSecretRef::Short(name) => secret_field(&secret_source_name(project, name, secrets)),
		ServiceSecretRef::Long {
			source,
			target,
			uid,
			gid,
			mode,
		} => {
			// `Secret=` is a comma-separated `key=value` option list, so a `,`
			// or `=` embedded in any field would inject extra options. Strip
			// those (and control chars) from each value at the boundary.
			let mut s = secret_field(&secret_source_name(project, source, secrets));
			if let Some(t) = target {
				s.push_str(&format!(",target={}", secret_field(t)));
			}
			if let Some(u) = uid {
				s.push_str(&format!(",uid={}", secret_field(u)));
			}
			if let Some(g) = gid {
				s.push_str(&format!(",gid={}", secret_field(g)));
			}
			if let Some(m) = mode {
				s.push_str(&format!(",mode={m:o}"));
			}
			s
		}
	}
}

/// Map a single compose `security_opt` entry onto the dedicated Quadlet key
/// where one exists; unrecognized entries are reported rather than dropped.
pub(super) fn map_security_opt(
	opt: &str,
	container: &mut Section,
	name: &str,
	warnings: &mut Vec<String>,
) {
	if let Some(rest) = opt.strip_prefix("no-new-privileges") {
		let val = rest.trim_start_matches([':', '=']);
		let enabled = val.is_empty() || val == "true";
		container.add("NoNewPrivileges", enabled.to_string());
	} else if let Some(profile) = opt.strip_prefix("seccomp=") {
		container.add("SeccompProfile", profile.to_string());
	} else if let Some(profile) = opt
		.strip_prefix("apparmor=")
		.or_else(|| opt.strip_prefix("apparmor:"))
	{
		// `AppArmor=` is not a recognised [Container] Quadlet key (Quadlet would
		// drop the whole unit at daemon-reload), so route it through PodmanArgs= as
		// `--security-opt apparmor=<profile>`, like the other escape-hatch flags.
		container.add("PodmanArgs", format!("--security-opt apparmor={profile}"));
	} else if let Some(label) = opt.strip_prefix("label=") {
		if label == "disable" {
			container.add("SecurityLabelDisable", "true".to_string());
		} else if label == "nested" {
			container.add("SecurityLabelNested", "true".to_string());
		} else if let Some(t) = label.strip_prefix("type:") {
			container.add("SecurityLabelType", t.to_string());
		} else if let Some(l) = label.strip_prefix("level:") {
			container.add("SecurityLabelLevel", l.to_string());
		} else if let Some(f) = label.strip_prefix("filetype:") {
			container.add("SecurityLabelFileType", f.to_string());
		} else {
			warnings.push(format!(
				"{name}: security_opt 'label={label}' has no Quadlet key and is skipped"
			));
		}
	} else if let Some(paths) = opt.strip_prefix("mask=") {
		container.add("Mask", paths.to_string());
	} else if let Some(paths) = opt.strip_prefix("unmask=") {
		container.add("Unmask", paths.to_string());
	} else {
		warnings.push(format!(
			"{name}: security_opt '{opt}' has no Quadlet mapping and is skipped"
		));
	}
}

#[cfg(test)]
#[path = "security_tests.rs"]
mod tests;
