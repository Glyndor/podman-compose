//! Removing the project's Podman-native secrets at `down`.
//!
//! Ownership is answered from one labelled list rather than an inspect per
//! secret (#1263). That list is also a superset of what the compose file names,
//! since every secret podup creates carries `podup.project=<proj>`, so a single
//! pass over it sweeps the declared secrets and the orphans alike.

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
		// same label — so every label was fetched twice, once per secret and once
		// for all of them.
		//
		// The label-carrying list is also a superset of what the compose loops
		// reach: every secret podup creates carries `podup.project=<proj>`, so
		// sweeping the labelled set covers the compose-named secrets and the
		// orphans (a key since renamed, or a `down` run without the original
		// file) in one pass. A same-named secret the user created by hand is not
		// in the set, which is exactly the guard this has to keep.
		match self.list_project_secret_names().await {
			Some(owned) => {
				// Ownership is already established by the label on each entry, so
				// these deletes are not re-inspected. The window between the list
				// and the last delete is wider than the old per-secret one; `down`
				// is best-effort here either way (a delete failure is logged, not
				// fatal), and a secret that changed hands inside it would have been
				// created by podup moments earlier.
				for name in owned {
					self.delete_listed_secret(&name).await;
				}
			}
			// The list failed. Falling through to an empty set would silently
			// delete nothing and report a clean teardown, so this drops back to
			// the per-secret guarded path instead — the same requests as before
			// #1263, only reached when the cheap route is unavailable. The orphan
			// sweep is not possible without a list, which is also how it behaved
			// before.
			None => {
				for (name, def) in &file.secrets {
					if is_podup_created_source(
						def.external,
						def.content.as_deref(),
						def.environment.as_deref(),
						def.file.as_deref(),
					) {
						self.delete_secret(&scoped_name(&self.project, "secret", name))
							.await;
					}
				}
				for (name, def) in &file.configs {
					if is_podup_created_source(
						def.external,
						def.content.as_deref(),
						def.environment.as_deref(),
						def.file.as_deref(),
					) {
						self.delete_secret(&scoped_name(&self.project, "config", name))
							.await;
					}
				}
			}
		}
		Ok(())
	}

	/// Names of all native secrets labelled `podup.project=<proj>` — the secrets
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
	async fn delete_listed_secret(&self, name: &str) {
		let path = format!("{API_PREFIX}/secrets/{}", urlencoded(name));
		match self.client.delete_ok(&path).await {
			Ok(()) => tracing::info!("removed secret {name}"),
			Err(e) => tracing::warn!("could not remove secret {name}: {e}"),
		}
	}

	/// Delete a project-scoped secret, but only after confirming it carries our
	/// `podup.project=<proj>` label — so a same-named secret the user created by
	/// hand (and which podup never created) is never destroyed on `down`. A
	/// missing secret (404) is a no-op.
	async fn delete_secret(&self, name: &str) {
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
						"secret {name} is not labelled podup.project={} — \
						 leaving it untouched (not created by podup)",
						self.project
					);
					return;
				}
			}
			Err(e) if e.is_status(404) => return,
			Err(e) => {
				tracing::warn!("could not inspect secret {name} before removal: {e}");
				return;
			}
		}
		let path = format!("{API_PREFIX}/secrets/{}", urlencoded(name));
		match self.client.delete_ok(&path).await {
			Ok(()) => tracing::info!("removed secret {name}"),
			Err(e) => tracing::warn!("could not remove secret {name}: {e}"),
		}
	}
}

#[cfg(test)]
mod tests {
	#[cfg(unix)]
	use crate::engine::fake_podman;
	#[cfg(unix)]
	use crate::engine::secrets::tests_support::{engine_on, file_with_content_secrets};

	/// A `/secrets/json` body holding one entry per `(name, project-label)` pair.
	#[cfg(unix)]
	fn secret_list(entries: &[(&str, &str)]) -> String {
		let items: Vec<String> = entries
			.iter()
			.map(|(name, project)| {
				format!(
					r#"{{"Spec":{{"Name":"{name}","Labels":{{"podup.project":"{project}"}}}}}}"#
				)
			})
			.collect();
		format!("[{}]", items.join(","))
	}

	/// #1263: the labelled list already answers the ownership question for every
	/// name at once, so teardown must not also inspect each secret individually
	/// for the same label. Measured on the six-secret bench scenario, dropping
	/// those takes `down -v` from 18 requests to 12.
	#[tokio::test]
	#[cfg(unix)]
	async fn down_uses_the_list_and_inspects_no_secret_individually() {
		let body = secret_list(&[("proj_secret_s1", "proj"), ("proj_secret_s2", "proj")]);
		let fake = fake_podman::start(move |method, target| {
			if method == "GET" && target.contains("/secrets/json") {
				(200, body.clone())
			} else {
				(200, "{}".to_string())
			}
		});
		let e = engine_on(&fake);

		e.remove_internal_secrets(&file_with_content_secrets(2))
			.await
			.expect("teardown should succeed");

		let seen = fake.requests.lock().unwrap().clone();
		let inspects: Vec<&String> = seen
			.iter()
			.filter(|r| r.starts_with("GET") && r.contains("/json") && !r.contains("/secrets/json"))
			.collect();
		assert!(
			inspects.is_empty(),
			"no per-secret inspect should be issued, got {inspects:?}"
		);
		assert_eq!(
			seen.iter().filter(|r| r.starts_with("DELETE")).count(),
			2,
			"both listed secrets are removed, got {seen:?}"
		);
	}

	/// The guard the batch has to keep: a secret carrying another project's label
	/// is not in the owned set, so it is neither inspected nor removed — even
	/// though the compose file names it.
	#[tokio::test]
	#[cfg(unix)]
	async fn down_never_deletes_a_secret_labelled_for_another_project() {
		let body = secret_list(&[
			("proj_secret_s1", "proj"),
			("proj_secret_s2", "someone-else"),
		]);
		let fake = fake_podman::start(move |method, target| {
			if method == "GET" && target.contains("/secrets/json") {
				(200, body.clone())
			} else {
				(200, "{}".to_string())
			}
		});
		let e = engine_on(&fake);

		e.remove_internal_secrets(&file_with_content_secrets(2))
			.await
			.expect("teardown should succeed");

		let seen = fake.requests.lock().unwrap().clone();
		assert!(
			seen.iter()
				.any(|r| r.starts_with("DELETE") && r.contains("proj_secret_s1")),
			"our own secret is removed, got {seen:?}"
		);
		assert!(
			!seen.iter().any(|r| r.contains("proj_secret_s2")),
			"a secret labelled for another project must not be touched at all, got {seen:?}"
		);
	}

	/// A secret podup created whose compose key was since renamed or removed is
	/// still swept, because the labelled list — not the compose file — is what
	/// teardown walks.
	#[tokio::test]
	#[cfg(unix)]
	async fn down_sweeps_an_orphan_the_compose_file_no_longer_names() {
		let body = secret_list(&[("proj_secret_gone", "proj")]);
		let fake = fake_podman::start(move |method, target| {
			if method == "GET" && target.contains("/secrets/json") {
				(200, body.clone())
			} else {
				(200, "{}".to_string())
			}
		});
		let e = engine_on(&fake);

		e.remove_internal_secrets(&file_with_content_secrets(1))
			.await
			.expect("teardown should succeed");

		let seen = fake.requests.lock().unwrap().clone();
		assert!(
			seen.iter()
				.any(|r| r.starts_with("DELETE") && r.contains("proj_secret_gone")),
			"an orphan carrying our label is still removed, got {seen:?}"
		);
	}

	/// The failure mode worth more than the saving. Since the list *is* the
	/// ownership check now, a failed list must not read as "nothing is ours" —
	/// that would delete nothing and report a clean `down`. It falls back to the
	/// per-secret guarded path instead.
	#[tokio::test]
	#[cfg(unix)]
	async fn a_failed_list_falls_back_to_per_secret_inspection_not_to_deleting_nothing() {
		let fake = fake_podman::start(|method, target| {
			if method == "GET" && target.contains("/secrets/json") {
				(500, r#"{"message":"boom"}"#.to_string())
			} else if method == "GET" {
				(
					200,
					r#"{"Spec":{"Labels":{"podup.project":"proj"}}}"#.to_string(),
				)
			} else {
				(200, "{}".to_string())
			}
		});
		let e = engine_on(&fake);

		e.remove_internal_secrets(&file_with_content_secrets(2))
			.await
			.expect("teardown should still succeed");

		let seen = fake.requests.lock().unwrap().clone();
		assert_eq!(
			seen.iter().filter(|r| r.starts_with("DELETE")).count(),
			2,
			"both compose-named secrets are still removed when the list fails, got {seen:?}"
		);
		assert!(
			seen.iter()
				.any(|r| r.starts_with("GET") && r.contains("proj_secret_s1/json")),
			"the fallback re-checks ownership per secret rather than assuming it, got {seen:?}"
		);
	}
}
