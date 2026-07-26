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
			self.create_secret(&name, &bytes).await?;
		}
		Ok(())
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
		match self.client.get_json::<serde_json::Value>(&inspect).await {
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
			}
			Err(e) if e.is_status(404) => {}
			Err(e) => return Err(ComposeError::Podman(e)),
		}
		let delete_path = format!("{API_PREFIX}/secrets/{}", urlencoded(name));
		self.client
			.delete_ok(&delete_path)
			.await
			.map_err(ComposeError::Podman)?;
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
			.map_err(ComposeError::Podman)
	}

	/// Remove the project-scoped native secrets created on `up` for the
	/// `content:`/`environment:`/`file:` secrets and configs, mirroring the volume
	/// and network teardown on `down`. `external:` references own no podup-created
	/// secret and are left untouched; a missing secret is ignored (`delete_ok`
	/// swallows a 404). Best-effort: a delete failure is logged, not fatal, so the
	/// rest of teardown proceeds.
	pub(super) async fn remove_internal_secrets(&self, file: &ComposeFile) -> Result<()> {
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
		// Catch orphans: a secret podup created on a previous `up` whose compose key
		// was since renamed/removed (or a `down` run without the original file) is
		// not reached by the loops above. Sweep every secret carrying this project's
		// label and remove it, so no podup-created secret is left behind.
		for name in self.list_project_secret_names().await {
			self.delete_secret(&name).await;
		}
		Ok(())
	}

	/// Names of all native secrets labelled `podup.project=<proj>` — the secrets
	/// podup created for this project. libpod's `/secrets/json` rejects a `label`
	/// filter (HTTP 500 `invalid filter "label"`), so the full list is fetched and
	/// filtered client-side by the `podup.project` label. Best-effort: a list
	/// failure yields an empty set so teardown still proceeds via the
	/// compose-driven deletes above.
	async fn list_project_secret_names(&self) -> Vec<String> {
		let path = format!("{API_PREFIX}/secrets/json");
		match self.client.get_json::<Vec<serde_json::Value>>(&path).await {
			Ok(list) => list
				.iter()
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
			Err(e) => {
				tracing::debug!("could not list project secrets for orphan cleanup: {e}");
				Vec::new()
			}
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
