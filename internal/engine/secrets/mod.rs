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
//! the read outright and `up` still reports the container as started — measured
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
//! did — a file bind pins the inode, so the write-new-and-rename that every
//! careful rotation tool performs was already invisible.
//!
//! The pure compose→plan mapping lives in [`plan`].

mod plan;

use std::collections::HashMap;
use std::path::Path;

use crate::compose::types::{ComposeFile, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::Secret;
use crate::libpod::{urlencoded, API_PREFIX};

use plan::{
	check_secret_size, collect_native_plans, host_file_secret_mode, is_podup_created_source,
	scoped_name, Payload,
};

use super::Engine;

impl Engine {
	/// Build the Podman-native secret references for a service. Every source podup
	/// creates — `content:`, `environment:` and `file:` — must already have been
	/// created by [`Engine::create_project_secrets`] (run once up front), so this
	/// only preflights `external: true` sources for existence — failing closed
	/// rather than starting a container that lacks the secret — and assembles the
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

	/// Create the union of the `content:`/`environment:`/`file:` secrets and
	/// configs declared across *all* services in the project, once, before the
	/// per-level start loop — mirroring how [`Engine::create_networks`] and
	/// [`Engine::create_volumes`] pre-create their resources.
	///
	/// Doing this up front fixes the race in which two services in the same
	/// dependency level (started concurrently) both ran the non-atomic
	/// delete-then-create for the same project-scoped secret name, so one could
	/// delete the secret the other had just created. The same scoped name is
	/// created exactly once here (later services share it), and each created
	/// secret carries the `podup.project=<proj>` label so the label-guarded
	/// teardown on `down` still only removes secrets podup owns.
	pub(super) async fn create_project_secrets(&self, file: &ComposeFile) -> Result<()> {
		let mut work: Vec<(String, Vec<u8>)> = Vec::new();
		for (name, payload) in collect_payload_union(&self.project, file, &self.base_dir)? {
			let bytes = match payload {
				Payload::Inline(bytes) => bytes,
				// Read here rather than in the planner, which stays free of I/O so
				// the compose→plan mapping remains unit-testable. The cap is the
				// same bounded read the compose-adjacent files get; Podman's own
				// 512 kB secret limit is enforced right after, in `create_secret`.
				Payload::File(path) => crate::filesystem::read_capped(&path).map_err(|e| {
					ComposeError::Unsupported(format!(
						"secret/config source {} could not be read: {e}",
						path.display()
					))
				})?,
			};
			work.push((name, bytes));
		}
		// The union is a `HashMap`, so its iteration order is arbitrary and not
		// stable between runs. Sorting by name is what makes "the first error
		// wins" mean something: without it, which of several failing secrets got
		// reported would vary run to run, and so would the test asserting it.
		work.sort_by(|(a, _), (b, _)| a.cmp(b));

		// Fanned out over the union, never per service (#1219). The union holds
		// distinct scoped names, so these chains cannot collide with each other;
		// parallelising per service instead would put two services in one
		// dependency level back to racing the same scoped name through a
		// non-atomic delete-then-create, which pre-creating the union once is
		// exactly what settles.
		//
		// Chunked `join_all` against the lifecycle's ceiling rather than its
		// `join_bounded`, and that is a compiler limitation rather than a
		// preference. `join_bounded` is built on `buffer_unordered`, whose
		// `FuturesUnordered` is `Send` only if its future is `Send` for *every*
		// lifetime. Reaching it from here — where the futures borrow `self` —
		// makes that bound higher-ranked and propagates out through `up` until an
		// unrelated `tokio::spawn(engine.watch(…))` stops compiling with
		// "implementation of `Send` is not general enough", reported in a test
		// file that never touches secrets. Owning the payloads and boxing the
		// futures were both tried; neither clears it. `join_all` over fixed-size
		// chunks carries no such bound, holds the same ceiling on simultaneous
		// libpod connections, and returns results in input order, so the error
		// reported is still the first by name.
		//
		// Note the behaviour change this carries: previously the first failing
		// secret aborted before the rest were touched, and now every secret in
		// its chunk is attempted. Nothing leaks — `down` sweeps by label — but a
		// failed `up` can leave later secrets created where it used to leave none.
		let mut results: Vec<Result<()>> = Vec::with_capacity(work.len());
		for chunk in work.chunks(crate::engine::lifecycle::parallel::MAX_LIFECYCLE_CONCURRENCY) {
			results.extend(
				futures_util::future::join_all(
					chunk
						.iter()
						.map(|(name, bytes)| self.create_secret(name, bytes)),
				)
				.await,
			);
		}
		match crate::engine::lifecycle::parallel::first_error(results) {
			Some(e) => Err(e),
			None => Ok(()),
		}
	}

	/// Create a Podman-native secret named `name` holding `payload`, labelled
	/// `podup.project=<proj>` so it can be cleaned up on `down`. The payload size
	/// is checked up front to turn Podman's opaque 500 into a clear message.
	///
	/// Idempotent across re-`up`s: rather than `replace=true` (which some Podman
	/// 5.x builds reject when the secret does not yet exist — the internal delete
	/// fails with "no secret data with ID"), the existing secret of this name is
	/// removed first (a 404 is fine) and then created fresh.
	async fn create_secret(&self, name: &str, payload: &[u8]) -> Result<()> {
		check_secret_size(name, payload.len())?;
		// Guard the delete-then-create: if a secret of this name already exists and
		// is not labelled as ours, refuse rather than clobber a foreign secret.
		// Our own secret (or a 404) is replaced fresh, keeping re-`up` idempotent.
		let inspect = format!("{API_PREFIX}/secrets/{}/json", urlencoded(name));
		let existed = match self.client.get_json::<serde_json::Value>(&inspect).await {
			Ok(info) => {
				let owned = info
					.get("Spec")
					.and_then(|spec| spec.get("Labels"))
					.and_then(|labels| labels.get("podup.project"))
					.and_then(|v| v.as_str())
					== Some(self.project.as_str());
				if !owned {
					return Err(ComposeError::Unsupported(format!(
						"a secret named '{name}' already exists and is not labelled \
						 podup.project={} — refusing to overwrite a secret podup did \
						 not create",
						self.project
					)));
				}
				true
			}
			Err(e) if e.is_status(404) => false,
			Err(e) => return Err(ComposeError::Podman(e)),
		};
		// Only delete something that is actually there (#1219). On a first `up`
		// every secret is a 404, so this was a round trip per secret spent
		// removing nothing.
		if existed {
			let delete_path = format!("{API_PREFIX}/secrets/{}", urlencoded(name));
			self.client
				.delete_ok(&delete_path)
				.await
				.map_err(ComposeError::Podman)?;
		}
		let labels = serde_json::json!({ "podup.project": self.project }).to_string();
		let path = format!(
			"{API_PREFIX}/secrets/create?name={}&labels={}",
			urlencoded(name),
			urlencoded(&labels),
		);
		// The response is `{"ID": "..."}`; we don't need the id, only success.
		self.client
			.post_bytes_json::<serde_json::Value>(
				&path,
				bytes::Bytes::copy_from_slice(payload),
				"application/octet-stream",
			)
			.await
			.map(|_| ())
			.map_err(|e| {
				// Skipping the delete opens a window: the inspect said the name was
				// free, and something claimed it before the create landed. `up` now
				// fails there instead of clobbering whatever arrived — the better
				// outcome, but only if it is legible. Podman's own message for this
				// is an opaque 500, so it is named here rather than passed through.
				if !existed {
					ComposeError::Unsupported(format!(
						"secret '{name}' did not exist when podup checked but could not \
						 be created: {e} — something else created a secret of that name \
						 in between. Re-run `up`, or remove it if it is not wanted."
					))
				} else {
					ComposeError::Podman(e)
				}
			})
	}

	/// Remove the project-scoped native secrets created on `up` for the
	/// `content:`/`environment:`/`file:` secrets and configs, mirroring the volume
	/// and network teardown on `down`. `external:` references own no podup-created
	/// secret and are left untouched; a missing secret is ignored (`delete_ok`
	/// swallows a 404). Best-effort: a delete failure is logged, not fatal, so the
	/// rest of teardown proceeds.
	pub(super) async fn remove_internal_secrets(&self, file: &ComposeFile) -> Result<()> {
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

#[cfg(test)]
mod tests {
	use super::*;
	use crate::libpod::Client;
	use std::path::PathBuf;

	#[cfg(unix)]
	use crate::engine::fake_podman;

	/// A compose file with `n` `content:` secrets named `s1..sn`, all on one
	/// service — the shape the union deduplicates and then fans out over.
	#[cfg(unix)]
	fn file_with_content_secrets(n: usize) -> ComposeFile {
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
	fn engine_on(fake: &fake_podman::FakePodman) -> Engine {
		Engine::with_base_dir(fake.client(), "proj".to_string(), std::env::temp_dir())
	}

	/// #1219: on a first `up` every secret inspect is a 404, so there is nothing
	/// to remove — the delete-then-create was spending a round trip per secret
	/// deleting a secret that does not exist. Measured on the six-secret bench
	/// scenario, this is what takes a cold `up` from 25 requests to 19.
	#[tokio::test]
	#[cfg(unix)]
	async fn create_skips_the_delete_when_the_secret_does_not_exist() {
		let fake = fake_podman::start(|method, target| {
			if method == "GET" && target.contains("/secrets/") {
				(404, r#"{"message":"no such secret"}"#.to_string())
			} else {
				(201, r#"{"ID":"abc"}"#.to_string())
			}
		});
		let e = engine_on(&fake);

		e.create_project_secrets(&file_with_content_secrets(3))
			.await
			.expect("creating fresh secrets should succeed");

		let seen = fake.requests.lock().unwrap().clone();
		let deletes: Vec<&String> = seen.iter().filter(|r| r.starts_with("DELETE")).collect();
		assert!(
			deletes.is_empty(),
			"no delete should be issued for a secret that does not exist, got {deletes:?}"
		);
		assert_eq!(
			seen.iter()
				.filter(|r| r.contains("/secrets/create"))
				.count(),
			3,
			"every secret is still created"
		);
	}

	/// The other half of the same rule: a secret that IS there still has to be
	/// removed before the create, because `replace=true` is rejected on some
	/// Podman 5.x builds. Skipping the delete unconditionally would break
	/// re-`up` idempotence, which is the reason the delete exists at all.
	#[tokio::test]
	#[cfg(unix)]
	async fn create_still_deletes_a_secret_that_already_exists() {
		let fake = fake_podman::start(|method, target| {
			if method == "GET" && target.contains("/secrets/") {
				(
					200,
					r#"{"Spec":{"Labels":{"podup.project":"proj"}}}"#.to_string(),
				)
			} else {
				(201, r#"{"ID":"abc"}"#.to_string())
			}
		});
		let e = engine_on(&fake);

		e.create_project_secrets(&file_with_content_secrets(2))
			.await
			.expect("replacing our own secrets should succeed");

		let seen = fake.requests.lock().unwrap().clone();
		assert_eq!(
			seen.iter().filter(|r| r.starts_with("DELETE")).count(),
			2,
			"each existing secret is removed before being recreated, got {seen:?}"
		);
	}

	/// The behaviour change the fan-out carries, stated in #1219 and asserted
	/// here rather than left to the reader: the pass no longer stops at the
	/// first failure, so every secret is attempted, and the error that surfaces
	/// is the first *by name* — not whichever chain happened to lose the race.
	#[tokio::test]
	#[cfg(unix)]
	async fn fan_out_attempts_every_secret_and_reports_the_first_by_name() {
		let fake = fake_podman::start(|method, target| {
			if method == "GET" && target.contains("/secrets/") {
				(404, r#"{"message":"no such secret"}"#.to_string())
			} else if method == "POST" && (target.contains("s2") || target.contains("s3")) {
				(500, r#"{"message":"boom"}"#.to_string())
			} else {
				(201, r#"{"ID":"abc"}"#.to_string())
			}
		});
		let e = engine_on(&fake);

		let err = e
			.create_project_secrets(&file_with_content_secrets(4))
			.await
			.expect_err("a failing secret must still fail the stage");

		let seen = fake.requests.lock().unwrap().clone();
		for i in 1..=4 {
			assert!(
				seen.iter().any(|r| r.contains(&format!("s{i}"))),
				"secret s{i} should have been attempted despite an earlier failure, got {seen:?}"
			);
		}
		assert!(
			err.to_string().contains("s2"),
			"the first failure by name should be the one reported, got: {err}"
		);
	}

	/// Skipping the delete opens a window between the inspect and the create.
	/// If something claims the name in it, `up` fails rather than clobbering
	/// what arrived — but Podman's own message for that is an opaque 500, so
	/// the failure has to name the race or the operator cannot act on it.
	#[tokio::test]
	#[cfg(unix)]
	async fn a_name_claimed_after_the_inspect_fails_with_a_legible_message() {
		let fake = fake_podman::start(|method, target| {
			if method == "GET" && target.contains("/secrets/") {
				(404, r#"{"message":"no such secret"}"#.to_string())
			} else {
				(500, r#"{"message":"secret name in use"}"#.to_string())
			}
		});
		let e = engine_on(&fake);

		let err = e
			.create_project_secrets(&file_with_content_secrets(1))
			.await
			.expect_err("a name claimed in the window must fail")
			.to_string();

		assert!(
			err.contains("in between"),
			"the message must explain the race, got: {err}"
		);
		assert!(
			err.contains("proj_secret_s1"),
			"the message must name which secret, got: {err}"
		);
		// The engine's own error is kept as the cause — it is useful — but it must
		// not be the whole of what the operator is handed, which is what the bare
		// `ComposeError::Podman` passthrough would have given them.
		assert!(
			!err.starts_with("podman API error"),
			"the raw engine error must not be the message itself, got: {err}"
		);
	}

	/// The ownership guard is untouched by any of the above: a secret of the
	/// same name that podup did not create is still refused, never deleted.
	#[tokio::test]
	#[cfg(unix)]
	async fn a_foreign_secret_is_still_refused_and_never_deleted() {
		let fake = fake_podman::start(|method, target| {
			if method == "GET" && target.contains("/secrets/") {
				(
					200,
					r#"{"Spec":{"Labels":{"podup.project":"someone-else"}}}"#.to_string(),
				)
			} else {
				(201, r#"{"ID":"abc"}"#.to_string())
			}
		});
		let e = engine_on(&fake);

		let err = e
			.create_project_secrets(&file_with_content_secrets(1))
			.await
			.expect_err("a foreign secret must not be overwritten")
			.to_string();

		assert!(err.contains("refusing to overwrite"), "got: {err}");
		let seen = fake.requests.lock().unwrap().clone();
		assert!(
			!seen.iter().any(|r| r.starts_with("DELETE")),
			"a foreign secret must never be deleted, got {seen:?}"
		);
	}

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

	fn engine_with_base(base: &str) -> Engine {
		Engine::with_base_dir(
			Client::new("unused"),
			"proj".to_string(),
			PathBuf::from(base),
		)
	}

	/// The path a `file:` payload will be read from, for the single planned secret.
	fn only_file_path(engine: &Engine, yaml: &str) -> PathBuf {
		let file = crate::compose::parse_str_raw(yaml).unwrap();
		let union = collect_payload_union("proj", &file, &engine.base_dir).unwrap();
		assert_eq!(union.len(), 1);
		match union.into_values().next().unwrap() {
			Payload::File(p) => p,
			Payload::Inline(_) => panic!("expected a file payload"),
		}
	}

	#[test]
	fn secret_file_relative_path_is_anchored_to_base_dir() {
		// A relative `file:` resolves against the project dir, not the Podman
		// service's cwd — same as a bind-mount source, which is what this was.
		let base = PathBuf::from("/srv/project");
		let yaml = "services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    file: secret.txt\n";
		let engine = engine_with_base(&base.to_string_lossy());
		assert_eq!(only_file_path(&engine, yaml), base.join("secret.txt"));
	}

	#[cfg(unix)]
	#[test]
	fn config_file_absolute_path_is_passed_through() {
		// Absolute paths are honored unchanged, exactly as `volumes:` does.
		let yaml = "services:\n  web:\n    image: nginx\n    configs: [cfg]\nconfigs:\n  cfg:\n    file: /etc/app/cfg.yaml\n";
		let engine = engine_with_base("/srv/project");
		assert_eq!(
			only_file_path(&engine, yaml),
			PathBuf::from("/etc/app/cfg.yaml")
		);
	}

	#[test]
	fn inline_union_dedups_shared_secret_across_services() {
		// Two services in the same project both reference the same inline secret.
		// The up-front union must create it once (one scoped name), not once per
		// service — which is what previously raced delete-then-create.
		let yaml = "services:\n  a:\n    image: nginx\n    secrets: [tok]\n  b:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    content: shared\n";
		let file = crate::compose::parse_str_raw(yaml).unwrap();
		let union = collect_payload_union("proj", &file, Path::new("/base")).unwrap();
		assert_eq!(union.len(), 1);
		assert!(matches!(
			union.get("proj_secret_tok"),
			Some(Payload::Inline(b)) if b == b"shared"
		));
	}

	#[test]
	fn payload_union_collects_every_source_podup_creates_but_not_external() {
		// The union spans secrets and configs across sources (distinct scoped names)
		// and excludes only `external:`, which podup never creates and must never
		// remove on `down`.
		let yaml = "services:\n  web:\n    image: nginx\n    secrets: [tok, ext, onfile]\n    configs: [cfg]\nsecrets:\n  tok:\n    content: s\n  ext:\n    external: true\n  onfile:\n    file: ./f.txt\nconfigs:\n  cfg:\n    content: c\n";
		let file = crate::compose::parse_str_raw(yaml).unwrap();
		let union = collect_payload_union("proj", &file, Path::new("/base")).unwrap();
		let mut names: Vec<&String> = union.keys().collect();
		names.sort();
		assert_eq!(
			names,
			vec!["proj_config_cfg", "proj_secret_onfile", "proj_secret_tok"]
		);
	}

	#[test]
	fn external_secret_is_never_in_the_payload_union() {
		// podup does not create an `external:` secret, so it must never appear in
		// the union that `up` creates and `down` removes.
		let yaml = "services:\n  web:\n    image: nginx\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n";
		let file = crate::compose::parse_str_raw(yaml).unwrap();
		let union = collect_payload_union("proj", &file, Path::new("/base")).unwrap();
		assert!(union.is_empty());
	}
}
