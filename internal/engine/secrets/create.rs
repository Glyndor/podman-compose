//! Creating the project's Podman-native secrets at `up`.
//!
//! The union of every `content:`/`environment:`/`file:` source across all
//! services is created once up front, before services start concurrently, so a
//! name two services share is never raced through the non-atomic
//! delete-then-create. Why `file:` sources are copied into native secrets at
//! all is in the module docs of [`super`].

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::plan::{check_secret_size, Payload};
use super::{collect_payload_union, Engine};

impl Engine {
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
	pub(in crate::engine) async fn create_project_secrets(&self, file: &ComposeFile) -> Result<()> {
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
	///
	/// # Concurrency contract — read before touching the inspect → delete → create sequence
	///
	/// The inspect-then-delete-then-create is **not** atomic at the libpod wire
	/// level, so a race in the window could delete something the caller does
	/// not own or land a state inconsistent with what the inspect claimed. Two
	/// guards close the cases that matter in practice:
	///
	/// 1. The **cross-invocation** case is closed by the per-project lock
	///    ([`crate::engine::lock`]) held for the duration of `up`. Two
	///    `podup` processes touching the same project serialise through it,
	///    so the inspect is never stale across processes.
	/// 2. The **within-invocation** case is closed by the project-scoped
	///    naming. Every secret created here has a name of the form
	///    `<project>_...`, which is unique to this `Engine` instance, and the
	///    inspect rejects any name that does not carry `podup.project=<proj>`
	///    — so a foreign secret of the same literal name is refused rather
	///    than clobbered. A race with the same project therefore cannot
	///    happen in the same process.
	///
	/// What these two guards do **not** cover: an external actor (a manual
	/// `podman secret create`, a separate compose stack, a test harness) that
	/// claims a project-scoped name in the window between inspect and create.
	/// That falls through to a `500 from libpod`, which this function
	/// recognises and rewraps into a legible "something else created a secret
	/// of that name in between" message — the operator can act on it without
	/// having to read the libpod error verbatim.
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
}

#[cfg(test)]
mod tests {
	#[cfg(unix)]
	use crate::engine::fake_podman;
	#[cfg(unix)]
	use crate::engine::secrets::tests_support::{engine_on, file_with_content_secrets};

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
}
