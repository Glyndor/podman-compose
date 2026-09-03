//! Hardening checks: one pure function per audit check id, each returning
//! every [`Finding`] a service raises under that check. Empty list = passed.
//!
//! Every check id is a stable snake_case string emitted into the JSON output
//! and into the `FINDINGS` column. Renaming one is a breaking change for
//! consumers; add new ids instead. `run_checks` is the single dispatch site
//! that turns the eleven named checks into a flat `Vec<Finding>`.

use podup::compose::types::{ComposeFile, Service};

use super::Finding;

/// Every check has the same shape: take the service name and the parsed
/// service plus the whole file (for cross-service context), return the
/// findings the service raises under this check. Pinned here so the
/// `run_checks` array stays trivial and a new check is one line plus one
/// function.
type CheckFn = fn(&str, &Service, &ComposeFile) -> Vec<Finding>;

/// Apply every registered check to one service, returning the union of all
/// findings. Kept as one function so a new check is a single edit: extend
/// the `check_*` vector at the bottom and the report picks it up.
///
/// `service_name` is the compose key; it is folded into each finding so the
/// renderer can group by service.
pub(super) fn run_checks(
	service_name: &str,
	service: &Service,
	file: &ComposeFile,
) -> Vec<Finding> {
	const CHECKS: [CheckFn; 11] = [
		check_privileged,
		check_host_namespace,
		check_dangerous_capability,
		check_writable_root,
		check_no_cap_drop_all,
		check_no_new_privileges_off,
		check_no_pids_limit,
		check_no_memory_limit,
		check_no_userns,
		check_secret_in_environment,
		check_unpinned_image,
	];
	let mut out = Vec::new();
	for check in CHECKS {
		out.extend(check(service_name, service, file));
	}
	out
}

/// Field-name substring matching for the `secret_in_environment` check:
/// `PASSWORD`, `SECRET`, `TOKEN`, `KEY`. Case-insensitive. The list is the
/// issue, so it lives in one place.
const SECRET_NAME_SUBSTRINGS: &[&str] = &["PASSWORD", "SECRET", "TOKEN", "KEY"];

/// `privileged: true`, grants extended host privileges that bypass the
/// default capability set. Under rootless Podman the effect is reduced but
/// the flag still means "give me more than the baseline"; it's never
/// incidental.
fn check_privileged(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	if service.privileged == Some(true) {
		vec![finding(
			name,
			"privileged",
			"privileged: true grants extended host privileges",
		)]
	} else {
		Vec::new()
	}
}

/// Host-binding namespacing modes: `pid`, `ipc`, `uts`, `cgroup`, `userns_mode`
/// set to `host`, or `network_mode: host`. One finding per active mode.
fn check_host_namespace(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	let mut out = Vec::new();
	if let Some(mode) = service.network_mode.as_deref() {
		if mode == "host" {
			out.push(finding(
				name,
				"host_namespace",
				"network_mode: host shares the host's network namespace",
			));
		}
	}
	for field in ["pid", "ipc", "uts", "cgroup", "userns_mode"] {
		let value = match field {
			"pid" => &service.pid,
			"ipc" => &service.ipc,
			"uts" => &service.uts,
			"cgroup" => &service.cgroup,
			"userns_mode" => &service.userns_mode,
			_ => unreachable!("checked field list"),
		};
		if let Some(mode) = value.as_deref() {
			if mode == "host" {
				out.push(finding(
					name,
					"host_namespace",
					&format!("{field}: host shares the host's {field} namespace"),
				));
			}
		}
	}
	out
}

/// `cap_add: [SYS_ADMIN]` or `cap_add: [ALL]`, Linux capabilities that
/// effectively grant root inside the container.
fn check_dangerous_capability(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	let mut out = Vec::new();
	for cap in &service.cap_add {
		let cap = normalized_capability(cap);
		if cap == "SYS_ADMIN" || cap == "ALL" {
			out.push(finding(
				name,
				"dangerous_capability",
				&format!("cap_add: {cap} grants root-equivalent capability"),
			));
		}
	}
	out
}

/// `read_only` not set to `true`, the container's rootfs is writable.
/// Compose's default is `false`; an absent key and an explicit `false` both
/// keep the filesystem writable, so the check fires unless the service
/// opted into read-only.
fn check_writable_root(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	if service.read_only != Some(true) {
		vec![finding(
			name,
			"writable_root",
			"read_only is not true: the container's root filesystem is writable",
		)]
	} else {
		Vec::new()
	}
}

/// `cap_drop` without `ALL`, the service can inherit a broader capability
/// set than it asked to drop. Spec asks for `ALL` (so the service starts
/// from nothing and opts back in via `cap_add:`).
fn check_no_cap_drop_all(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	if !service
		.cap_drop
		.iter()
		.any(|c| normalized_capability(c) == "ALL")
	{
		vec![finding(
			name,
			"no_cap_drop_all",
			"cap_drop does not contain ALL: the service keeps the runtime's default capability set",
		)]
	} else {
		Vec::new()
	}
}

/// `security_opt` without `no-new-privileges:true`. Podman spells it
/// `no-new-privileges` (no `:true`); both spellings are accepted. The check
/// iterates each entry and looks for a prefix match on either spelling; a
/// bare `no-new-privileges` (no value) also passes.
fn check_no_new_privileges_off(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	// Podman spells it `no-new-privileges` alone or `no-new-privileges:true`;
	// `no-new-privileges:false` is the option being switched off, not on.
	let has = service.security_opt.iter().any(|opt| {
		let mut parts = opt.splitn(2, ':');
		let head = parts.next().unwrap_or(opt);
		let value = parts.next().unwrap_or("true");
		head == "no-new-privileges" && value == "true"
	});
	if has {
		Vec::new()
	} else {
		vec![finding(
			name,
			"no_new_privileges_off",
			"security_opt is missing no-new-privileges:true: setuid binaries may regain privileges",
		)]
	}
}

/// `pids_limit` unset, no ceiling on the number of processes the container
/// can fork, so a runaway loop can starve the host out of PIDs.
fn check_no_pids_limit(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	if service.pids_limit.is_none() {
		vec![finding(
			name,
			"no_pids_limit",
			"pids_limit is not set: a fork bomb can exhaust the host's process table",
		)]
	} else {
		Vec::new()
	}
}

/// Neither `mem_limit` nor `deploy.resources.limits.memory` set, no upper
/// bound on memory. A misbehaving service can OOM the host.
fn check_no_memory_limit(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	let deploy_limit = service
		.deploy
		.as_ref()
		.and_then(|d| d.resources.as_ref())
		.and_then(|r| r.limits.as_ref())
		.and_then(|l| l.memory.as_ref());
	if service.mem_limit.is_none() && deploy_limit.is_none() {
		vec![finding(
			name,
			"no_memory_limit",
			"neither mem_limit nor deploy.resources.limits.memory is set: a leak can OOM the host",
		)]
	} else {
		Vec::new()
	}
}

/// `userns_mode` unset, Podman's `auto` (the default behaviour when the
/// field is absent) gives each container its own UID range; an explicit
/// keep-id/host is rare and is what we want the operator to confirm.
fn check_no_userns(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	if service.userns_mode.is_none() {
		vec![finding(
			name,
			"no_userns",
			"userns_mode is not set: with `auto` Podman gives each container its own range of subordinate UIDs; see docs/docker-migration.md",
		)]
	} else {
		Vec::new()
	}
}

/// `environment:` key whose name contains `PASSWORD|SECRET|TOKEN|KEY`
/// (case-insensitive) with a literal non-empty value. Bare keys (host
/// inheritance) and unresolved `${VAR}` placeholders are not secrets.
///
/// Service-local: the check is a positional grep on the `environment:` map
/// of this service. These are surfaced so the operator can
/// move them to `secrets:`; the wider question of whether the project
/// declares `secrets:` is not in scope.
fn check_secret_in_environment(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	let mut out = Vec::new();
	for (key, value) in service.environment.to_map() {
		let upper = key.to_ascii_uppercase();
		if !SECRET_NAME_SUBSTRINGS.iter().any(|s| upper.contains(s)) {
			continue;
		}
		let Some(value) = value else {
			// Bare key: inherited from the host. Not a published secret.
			continue;
		};
		if value.is_empty() {
			// Empty literal: probably a placeholder; `docker compose` does
			// not raise this either.
			continue;
		}
		if value.starts_with("${") && value.ends_with('}') {
			// Unresolved ${VAR} placeholder: not a published secret either.
			continue;
		}
		out.push(finding(
			name,
			"secret_in_environment",
			&format!("environment: {key} carries a hard-coded value; move it to secrets:"),
		));
	}
	// The `to_map` for the `Empty` enum yields nothing, so the unused
	// `match arms` below are defensive: future variants of `EnvVars` ought
	// to keep the same shape.
	out
}

/// `image:` is unpinned: no tag (defaults to `latest`), tag is `latest`,
/// or `latest` is not pinned by a digest. An `@sha256:` digest counts as
/// pinned even when a tag is also present.
fn check_unpinned_image(name: &str, service: &Service, _file: &ComposeFile) -> Vec<Finding> {
	let Some(reference) = service.image.as_deref() else {
		// A `build:` service without an `image:` is built from source and
		// has no registry reference to pin; out of scope for this check.
		return Vec::new();
	};
	if reference.contains('@') {
		// Any digest (sha256: or otherwise) anchors the tag, so the service
		// is pinned regardless of the tag's value.
		return Vec::new();
	}
	let last_colon = reference.rfind(':').unwrap_or(0);
	let last_slash = reference.rfind('/').unwrap_or(0);
	// The tag/separator sits after the last colon not preceded by a slash;
	// an image with no tag stops there.
	let has_tag = last_colon > last_slash;
	if !has_tag {
		return vec![finding(
			name,
			"unpinned_image",
			&format!("image: {reference} has no tag; defaults to :latest"),
		)];
	}
	let tag = &reference[last_colon + 1..];
	if tag == "latest" {
		return vec![finding(
			name,
			"unpinned_image",
			&format!("image: {reference} pins to :latest, which moves under you"),
		)];
	}
	Vec::new()
}

/// Construct one [`Finding`]. Private to this module so callers always go
/// through `run_checks`, and so the field-name ordering (service, check,
/// reason) is consistent across all checks.
/// `CAP_SYS_ADMIN`, `sys_admin` and `SYS_ADMIN` name the same capability;
/// compose files carry all three spellings.
fn normalized_capability(cap: &str) -> String {
	let upper = cap.trim().to_ascii_uppercase();
	upper.strip_prefix("CAP_").unwrap_or(&upper).to_string()
}

fn finding(name: &str, check: &'static str, reason: &str) -> Finding {
	Finding {
		service: name.to_string(),
		check,
		reason: reason.to_string(),
	}
}

#[cfg(test)]
#[path = "checks_more_tests.rs"]
mod more_tests;
#[cfg(test)]
#[path = "checks_tests.rs"]
mod tests;
