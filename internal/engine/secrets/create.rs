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
use super::secret_bytes::SecretBytes;
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
		let mut work: Vec<(String, SecretBytes)> = Vec::new();
		for (name, payload) in collect_payload_union(&self.project, file, &self.base_dir)? {
			let bytes = match payload {
				Payload::Inline(bytes) => bytes,
				// Read here rather than in the planner, which stays free of I/O so
				// the compose→plan mapping remains unit-testable. The cap is the
				// same bounded read the compose-adjacent files get; Podman's own
				// 512 kB secret limit is enforced right after, in `create_secret`.
				Payload::File(path) => {
					SecretBytes::new(crate::filesystem::read_capped(&path).map_err(|e| {
						ComposeError::Unsupported(format!(
							"secret/config source {} could not be read: {e}",
							path.display()
						))
					})?)
				}
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
	async fn create_secret(&self, name: &str, payload: &SecretBytes) -> Result<()> {
		check_secret_size(name, payload.byte_len())?;
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
				bytes::Bytes::copy_from_slice(payload.expose_secret()),
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
#[path = "create_tests.rs"]
mod tests;

/// The payload of a secret must never reach an error message.
///
/// CodeQL raised seven `rust/cleartext-logging` alerts against
/// `tests/engine_integration/`, all with the same source:
/// `self.create_project_secrets(...)` flowing into an `assert!` message. The
/// claim is worth answering rather than dismissing, because the answer is a
/// property of this file and nothing was checking it.
///
/// It holds today. `create_project_secrets` returns `Result<()>`, so the bytes
/// are not in the type a caller can print, and the one error it builds itself
/// carries `path.display()` and the underlying I/O error rather than the
/// payload. What was missing is anything that keeps it true: every variant of
/// `ComposeError` on this path carries a `String`, and a future edit that
/// formats the payload into one would leak it into a public CI log, where the
/// integration tests print the error on failure.
///
/// The marker is deliberately long and unlike anything else in the tree, so a
/// substring match cannot pass by coincidence.
#[cfg(test)]
#[cfg(unix)]
#[path = "create_payload_never_reaches_an_error.rs"]
mod payload_never_reaches_an_error;
