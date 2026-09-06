//! Secret and config injection.
//!
//! Every source is injected as a Podman-native secret attached to the container
//! create spec:
//!
//! * inline `content:`/`environment:` and `file:` → created over the libpod API
//!   (`secrets/create`, removing any prior secret of the name first so a re-`up`
//!   is idempotent) under a project-scoped name, so nothing is written to a host
//!   staging directory. The project's whole payload union is created once up
//!   front by [`Engine::create_project_secrets`] (before services start
//!   concurrently), not per-service, so a shared name is never raced.
//! * `external: true` → mapped to a pre-existing `podman secret`, preflighted
//!   with [`Engine::ensure_external_exists`] so a missing secret fails closed.
//!
//! `file:` sources used to be read-only bind mounts of the host path instead.
//! That worked until the host enforced SELinux, where the container is denied
//! the read outright and `up` still reports the container as started, measured
//! on Fedora with both supported Podman majors, and reproduced by plain `podman
//! run`, so the denial was the missing relabel and not podup. Relabelling was
//! the other way out, but `z` rewrites the label of a file the user owns and may
//! share with a confined host service, and compose gives them nowhere to ask for
//! it. Reading the bytes into a native secret leaves the host untouched and puts
//! `file:` on the path the other two sources already took. What the container
//! sees is unchanged: the mount mode mirrors the host file's own bits (see
//! [`plan::host_file_secret_mode`]) rather than defaulting to `0444`.
//!
//! The trade is that the payload is a copy taken at `up`, so an in-place edit of
//! the host file no longer reaches a running container. An atomic replace never
//! did: a file bind pins the inode, so the write-new-and-rename that every
//! careful rotation tool performs was already invisible.
//!
//! The pure compose→plan mapping lives in [`plan`].

mod create;
mod plan;
mod remove;
mod secret_bytes;

use std::collections::HashMap;
use std::path::Path;

use crate::compose::types::{ComposeFile, Service};
use crate::error::Result;
use crate::libpod::types::container::Secret;

use plan::{collect_native_plans, host_file_secret_mode, Payload};

use super::Engine;

impl Engine {
	/// Build the Podman-native secret references for a service. Every source podup
	/// creates (`content:`, `environment:` and `file:`) must already have been
	/// created by [`Engine::create_project_secrets`] (run once up front), so this
	/// only preflights `external: true` sources for existence, failing closed
	/// rather than starting a container that lacks the secret, and assembles the
	/// per-service references attached to the container spec.
	///
	/// Creation is deliberately *not* done here: services in the same
	/// dependency level are brought up concurrently, and a per-service
	/// delete-then-create on a shared secret name would race (one create could
	/// clobber a secret another service's container is about to use). The up-front
	/// pass creates each secret exactly once instead.
	pub(super) async fn build_native_secrets(
		&self,
		service: &Service,
		file: &ComposeFile,
	) -> Result<Vec<Secret>> {
		let plans = collect_native_plans(&self.project, service, file, &self.base_dir)?;
		let mut secrets = Vec::with_capacity(plans.len());
		for plan in plans {
			// Payloads podup owns are created up front; only external sources need a
			// (read-only, idempotent) existence preflight here.
			if plan.payload.is_none() {
				self.ensure_external_exists("secret", "secrets", &plan.source)
					.await?;
			}
			// A `file:` source with no explicit `mode:` mounts with the host file's
			// own bits, so what the container sees does not change now that the file
			// is copied into a native secret rather than bind-mounted.
			let mode = match (&plan.payload, plan.mode) {
				(Some(Payload::File(path)), None) => Some(host_file_secret_mode(path)),
				_ => plan.mode,
			};
			secrets.push(Secret {
				source: plan.source,
				target: Some(plan.target),
				uid: plan.uid,
				gid: plan.gid,
				mode,
			});
		}
		Ok(secrets)
	}
}

/// Collect the project's podup-created secret/config payloads, deduplicated by
/// their scoped Podman secret name.
///
/// The same secret referenced by several services resolves to one project-scoped
/// name, so it is created once and shared. A first writer wins: every reference
/// to a given name yields the identical payload (inline bytes and `file:` paths
/// alike come from the single compose def), so the dedup is value-stable. No
/// daemon access and no file reads, so the union and its dedup are unit-testable.
fn collect_payload_union(
	project: &str,
	file: &ComposeFile,
	base_dir: &Path,
) -> Result<HashMap<String, Payload>> {
	let mut payloads: HashMap<String, Payload> = HashMap::new();
	for service in file.services.values() {
		for plan in collect_native_plans(project, service, file, base_dir)? {
			if let Some(payload) = plan.payload {
				payloads.entry(plan.source).or_insert(payload);
			}
		}
	}
	Ok(payloads)
}

/// Helpers the `create` and `remove` test modules share.
#[cfg(test)]
pub(super) mod tests_support {
	use super::*;
	#[cfg(unix)]
	use crate::engine::fake_podman;

	/// A compose file with `n` `content:` secrets named `s1..sn`, all on one
	/// service, the shape the union deduplicates and then fans out over.
	#[cfg(unix)]
	pub(in crate::engine::secrets) fn file_with_content_secrets(n: usize) -> ComposeFile {
		let refs: String = (1..=n).map(|i| format!("      - s{i}\n")).collect();
		let defs: String = (1..=n)
			.map(|i| format!("  s{i}: {{content: \"v{i}\"}}\n"))
			.collect();
		crate::compose::parse_str(&format!(
			"services:\n  app:\n    image: alpine\n    secrets:\n{refs}secrets:\n{defs}"
		))
		.expect("fixture compose file should parse")
	}

	#[cfg(unix)]
	pub(in crate::engine::secrets) fn engine_on(fake: &fake_podman::FakePodman) -> Engine {
		Engine::with_base_dir(fake.client(), "proj".to_string(), std::env::temp_dir())
	}
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
