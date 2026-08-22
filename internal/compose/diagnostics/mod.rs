//! Parse-time diagnostics.
//!
//! podup accepts the full compose-spec surface and, per the spec's
//! forward-compatibility rule, never treats an unknown key as a hard error.
//! The cost of that leniency is silent drops: a typo or an unmapped compose
//! feature would just vanish. This pass closes that gap by reporting every key
//! or field podup parses but cannot translate, so nothing is ignored without
//! the operator hearing about it — the same guarantee that lets podup absorb
//! future Docker/Podman compose additions gracefully.

use super::types::ComposeFile;

mod ignored_fields;
mod nested_raw;
use ignored_fields::{
	ignored_build_fields, ignored_models, ignored_network_fields, ignored_port_fields,
	ignored_restart_policy_fields, ignored_secret_config_drivers, ignored_service_fields,
	ignored_service_network_fields, ignored_volume_mount_fields, port_published_on_all_interfaces,
};
pub(super) use nested_raw::raw_nested_unknown_warnings;

/// Collect a warning for every parsed-but-unsupported key or field in `file`.
/// Pure (no logging) so it can be unit-tested; the caller emits the messages.
pub(super) fn collect(file: &ComposeFile) -> Vec<String> {
	let mut out = Vec::new();
	unknown_top_level_keys(file, &mut out);
	unknown_service_keys(file, &mut out);
	nested_unknown_keys(file, &mut out);
	ignored_service_fields(file, &mut out);
	ignored_port_fields(file, &mut out);
	port_published_on_all_interfaces(file, &mut out);
	ignored_volume_mount_fields(file, &mut out);
	ignored_build_fields(file, &mut out);
	ignored_network_fields(file, &mut out);
	ignored_service_network_fields(file, &mut out);
	ignored_secret_config_drivers(file, &mut out);
	ignored_models(file, &mut out);
	ignored_restart_policy_fields(file, &mut out);
	out
}

/// Push a warning for each non-`x-` key captured at a nested level.
fn push_unknown(
	context: &str,
	unknown: &indexmap::IndexMap<String, serde_yaml::Value>,
	out: &mut Vec<String>,
) {
	for key in unknown.keys() {
		if key.starts_with("x-") {
			continue;
		}
		out.push(format!(
			"{context}: unknown key '{key}' is ignored \
			 (check for a typo or an unsupported compose feature)"
		));
	}
}

/// Unknown keys captured inside service sub-objects and top-level network /
/// volume definitions. Together with the service- and top-level passes this
/// means a typo or an unmapped future field at ANY modeled level is surfaced
/// rather than silently dropped.
fn nested_unknown_keys(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		if let Some(hc) = &def.healthcheck {
			push_unknown(
				&format!("service '{service}' healthcheck"),
				&hc.unknown,
				out,
			);
		}
		if let Some(deploy) = &def.deploy {
			push_unknown(&format!("service '{service}' deploy"), &deploy.unknown, out);
		}
		if let Some(develop) = &def.develop {
			for (i, rule) in develop.watch.iter().enumerate() {
				push_unknown(
					&format!("service '{service}' develop.watch[{i}]"),
					&rule.unknown,
					out,
				);
			}
		}
		if let Some(cred) = &def.credential_spec {
			push_unknown(
				&format!("service '{service}' credential_spec"),
				&cred.unknown,
				out,
			);
		}
		if let Some(provider) = &def.provider {
			push_unknown(
				&format!("service '{service}' provider"),
				&provider.unknown,
				out,
			);
		}
	}
	for (name, model) in &file.models {
		push_unknown(&format!("model '{name}'"), &model.unknown, out);
	}
	for (name, cfg) in &file.networks {
		if let Some(c) = cfg {
			push_unknown(&format!("network '{name}'"), &c.unknown, out);
			if let Some(ipam) = &c.ipam {
				push_unknown(&format!("network '{name}' ipam"), &ipam.unknown, out);
			}
		}
	}
	for (name, cfg) in &file.volumes {
		if let Some(c) = cfg {
			push_unknown(&format!("volume '{name}'"), &c.unknown, out);
		}
	}
}

/// Top-level keys that matched no known field (captured in `extensions`).
/// `x-*` extension keys are allowed by the spec and skipped.
fn unknown_top_level_keys(file: &ComposeFile, out: &mut Vec<String>) {
	for key in file.extensions.keys() {
		if key.starts_with("x-") {
			continue;
		}
		out.push(format!(
			"unknown top-level key '{key}' is ignored \
			 (check for a typo or an unsupported compose feature)"
		));
	}
}

/// Service keys that matched no known field. A likely typo — e.g.
/// `enviroment:` — is easy to miss when it just vanishes, so surface it.
fn unknown_service_keys(file: &ComposeFile, out: &mut Vec<String>) {
	for (service, def) in &file.services {
		for key in def.unknown.keys() {
			if key.starts_with("x-") {
				continue;
			}
			out.push(format!(
				"service '{service}': unknown key '{key}' is ignored \
				 (check for a typo or an unsupported compose feature)"
			));
		}
	}
}

#[cfg(test)]
mod tests;
