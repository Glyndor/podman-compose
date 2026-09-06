//! Removing the project's Podman-native secrets at `down`.
//!
//! Ownership is answered from one labelled list rather than an inspect per
//! secret (#1263). That list is also a superset of what the compose file names,
//! since every secret podup creates carries `podup.project=<proj>`, so a single
//! pass over it sweeps the declared secrets and the orphans alike.
//!
//! Every secret podup creates, including `file:` sources, whose bytes are
//! copied into a Podman-native secret at `up` time and **persist independently
//! of any container** (`podman-secret-rm(1)`: the secret lives in the podman
//! store, not in container state), is removed on `down`. Without this sweep
//! the bytes from a `file:` source survive a `down`/`up` cycle until the next
//! `up` overwrites them, which is a defence-in-depth gap (#1360): a stale
//! secret payload outlives the container that read it. A summary log line
//! names the secrets that were removed, so the operator can see the teardown
//! actually happened.

use crate::compose::types::ComposeFile;
use crate::error::Result;
use crate::libpod::{urlencoded, API_PREFIX};

use super::plan::{is_podup_created_source, scoped_name};
use super::Engine;

impl Engine {
	/// Remove the project-scoped native secrets created on `up` for the
	/// `content:`/`environment:`/`file:` secrets and configs, mirroring the volume
	/// and network teardown on `down`. `external:` references own no podup-created
	/// secret and are left untouched; a missing secret is ignored (`delete_ok`
	/// swallows a 404). Best-effort: a delete failure is logged, not fatal, so the
	/// rest of teardown proceeds.
	pub(in crate::engine) async fn remove_internal_secrets(
		&self,
		file: &ComposeFile,
	) -> Result<()> {
		// One list answers the ownership question for every name at once (#1263).
		// It used to be fetched only for the orphan sweep, *after* each
		// compose-named secret had already been inspected individually for the
		// same label, so every label was fetched twice, once per secret and once
		// for all of them.
		//
		// The label-carrying list is also a superset of what the compose loops
		// reach: every secret podup creates carries `podup.project=<proj>`, so
		// sweeping the labelled set covers the compose-named secrets and the
		// orphans (a key since renamed, or a `down` run without the original
		// file) in one pass. A same-named secret the user created by hand is not
		// in the set, which is exactly the guard this has to keep.
		//
		// Both paths emit a single summary log line so the operator can see
		// which secrets were removed (#1360 L6). The per-secret `info` lines
		// stay; the summary is the closure the user is actually looking for
		// when the teardown finishes.
		match self.list_project_secret_names().await {
			Some(owned) => {
				// Ownership is already established by the label on each entry, so
				// these deletes are not re-inspected. The window between the list
				// and the last delete is wider than the old per-secret one; `down`
				// is best-effort here either way (a delete failure is logged, not
				// fatal), and a secret that changed hands inside it would have been
				// created by podup moments earlier.
				let mut removed = Vec::with_capacity(owned.len());
				for name in owned {
					if self.delete_listed_secret(&name).await {
						removed.push(name);
					}
				}
				log_removed_secrets(&self.project, &removed);
			}
			// The list failed. Falling through to an empty set would silently
			// delete nothing and report a clean teardown, so this drops back to
			// the per-secret guarded path instead, the same requests as before
			// #1263, only reached when the cheap route is unavailable. The orphan
			// sweep is not possible without a list, which is also how it behaved
			// before.
			None => {
				let mut removed = Vec::new();
				for (name, def) in &file.secrets {
					if is_podup_created_source(
						def.external,
						def.content.as_deref(),
						def.environment.as_deref(),
						def.file.as_deref(),
					) {
						let scoped = scoped_name(&self.project, "secret", name);
						if self.delete_secret(&scoped).await {
							removed.push(scoped);
						}
					}
				}
				for (name, def) in &file.configs {
					if is_podup_created_source(
						def.external,
						def.content.as_deref(),
						def.environment.as_deref(),
						def.file.as_deref(),
					) {
						let scoped = scoped_name(&self.project, "config", name);
						if self.delete_secret(&scoped).await {
							removed.push(scoped);
						}
					}
				}
				log_removed_secrets(&self.project, &removed);
			}
		}
		Ok(())
	}

	/// Names of all native secrets labelled `podup.project=<proj>`, the secrets
	/// podup created for this project. libpod's `/secrets/json` rejects a `label`
	/// filter (HTTP 500 `invalid filter "label"`), so the full list is fetched and
	/// filtered client-side by the `podup.project` label.
	///
	/// `None` means the list could not be fetched, and is deliberately not the
	/// same value as `Some(vec![])`. Since #1263 this list *is* the ownership
	/// check for teardown, so collapsing a failure into "no secrets are ours"
	/// would delete nothing and call it a clean `down`. The caller falls back to
	/// inspecting each compose-named secret instead.
	async fn list_project_secret_names(&self) -> Option<Vec<String>> {
		let path = format!("{API_PREFIX}/secrets/json");
		match self.client.get_json::<Vec<serde_json::Value>>(&path).await {
			Ok(list) => Some(
				list.iter()
					.filter_map(|s| {
						let spec = s.get("Spec")?;
						let owned = spec
							.get("Labels")
							.and_then(|l| l.get("podup.project"))
							.and_then(|v| v.as_str())
							== Some(self.project.as_str());
						if owned {
							spec.get("Name")
								.and_then(|n| n.as_str())
								.map(str::to_string)
						} else {
							None
						}
					})
					.collect(),
			),
			Err(e) => {
				tracing::debug!(
					"could not list project secrets, falling back to per-secret inspection: {e}"
				);
				None
			}
		}
	}

	/// Delete a secret whose `podup.project=<proj>` label was already confirmed
	/// by [`Self::list_project_secret_names`], so it carries no inspect of its
	/// own. Kept separate from [`Self::delete_secret`] rather than adding a flag,
	/// so that a call site can never accidentally skip a check it was supposed to
	/// make: this one is only reachable from a name the list vouched for.
	///
	/// Returns `true` when the secret was actually removed (or was already
	/// absent; a 404 still counts as teardown success). The caller collects
	/// the names for the summary log line.
	async fn delete_listed_secret(&self, name: &str) -> bool {
		let path = format!("{API_PREFIX}/secrets/{}", urlencoded(name));
		match self.client.delete_ok(&path).await {
			Ok(()) => {
				tracing::info!("removed secret {name}");
				true
			}
			Err(e) => {
				tracing::warn!("could not remove secret {name}: {e}");
				false
			}
		}
	}

	/// Delete a project-scoped secret, but only after confirming it carries our
	/// `podup.project=<proj>` label, so a same-named secret the user created by
	/// hand (and which podup never created) is never destroyed on `down`. A
	/// missing secret (404) is a no-op.
	///
	/// Returns `true` when the secret was actually removed, `false` when the
	/// inspect failed (404, transport error, or a foreign label) and the
	/// caller should not count it as teardown.
	async fn delete_secret(&self, name: &str) -> bool {
		let inspect = format!("{API_PREFIX}/secrets/{}/json", urlencoded(name));
		match self.client.get_json::<serde_json::Value>(&inspect).await {
			Ok(info) => {
				let owned = info
					.get("Spec")
					.and_then(|spec| spec.get("Labels"))
					.and_then(|labels| labels.get("podup.project"))
					.and_then(|v| v.as_str())
					== Some(self.project.as_str());
				if !owned {
					tracing::warn!(
						"secret {name} is not labelled podup.project={}, \
						 leaving it untouched (not created by podup)",
						self.project
					);
					return false;
				}
			}
			Err(e) if e.is_status(404) => return false,
			Err(e) => {
				tracing::warn!("could not inspect secret {name} before removal: {e}");
				return false;
			}
		}
		let path = format!("{API_PREFIX}/secrets/{}", urlencoded(name));
		match self.client.delete_ok(&path).await {
			Ok(()) => {
				tracing::info!("removed secret {name}");
				true
			}
			Err(e) => {
				tracing::warn!("could not remove secret {name}: {e}");
				false
			}
		}
	}
}

/// One summary log line that lists the podup-created secrets removed on
/// `down`. The per-secret `info` lines stay as the per-record audit;
/// the summary is the line the operator actually searches for when
/// the teardown finishes. Empty lists are silent: a `down` with no
/// declared secrets composes cleanly with the rest of the teardown
/// without adding noise.
fn log_removed_secrets(project: &str, removed: &[String]) {
	if removed.is_empty() {
		return;
	}
	tracing::info!(
		"removed {} podup-created secret(s) for project {project}: {}",
		removed.len(),
		removed.join(", ")
	);
}

#[cfg(test)]
#[path = "remove_tests.rs"]
mod tests;
