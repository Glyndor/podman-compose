//! Shared healthcheck readiness for the concurrent `up` path.
//!
//! When several services in a dependency level declare `depends_on: <svc>:
//! {condition: service_healthy}`, they start concurrently and would each poll
//! that container's healthcheck, and every poll *runs* the check inside the
//! container, so a service N others wait on gets its healthcheck executed ~N×
//! per interval for the whole startup. [`Engine::build_readiness_map`] memoizes
//! one poller per container so the check runs once per interval regardless of
//! how many depend on it.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::future::{FutureExt, Shared};

use crate::compose::types::{ComposeFile, ServiceCondition};
use crate::engine::Engine;
use crate::error::ComposeError;

use super::targets::in_started_set;

/// A `wait_healthy` future shared across every dependent of one container, so a
/// service that N others wait on has its healthcheck polled by a single poller
/// rather than ~N× per interval. Lazy: the poll begins when the first dependent
/// awaits it. The error is `Arc`-wrapped because [`Shared`] needs a `Clone`
/// output and [`ComposeError`] is not `Clone`.
pub(super) type SharedReady<'a> =
	Shared<Pin<Box<dyn Future<Output = std::result::Result<(), Arc<ComposeError>>> + Send + 'a>>>;

impl Engine {
	/// Build one shared readiness future per container that any starting service
	/// waits on with `condition: service_healthy`.
	///
	/// The predicate mirrors the wait guard in `up_one_service`; a container it
	/// misses simply falls back to a direct wait there, so a mismatch degrades to
	/// the old per-dependent behaviour rather than a panic.
	pub(super) fn build_readiness_map<'a>(
		&'a self,
		file: &'a ComposeFile,
		enabled: &HashSet<String>,
		target_set: &Option<HashSet<String>>,
		start: bool,
	) -> HashMap<String, SharedReady<'a>> {
		let mut map: HashMap<String, SharedReady<'a>> = HashMap::new();
		// `create` (start = false) gates on nothing, so there are no waits to share.
		if !start {
			return map;
		}
		for (sname, service) in &file.services {
			// Only services this pass actually starts run their readiness waits.
			if let Some(set) = target_set {
				if !set.contains(sname) {
					continue;
				}
			}
			if !enabled.contains(sname) {
				continue;
			}
			for dep in service.depends_on.service_names() {
				if !matches!(
					service.depends_on.condition_for(&dep),
					ServiceCondition::ServiceHealthy
				) {
					continue;
				}
				if !in_started_set(target_set, &dep) {
					continue;
				}
				let Some(dep_service) = file.services.get(&dep) else {
					continue;
				};
				if !enabled.contains(&dep) {
					continue;
				}
				// A disabled healthcheck is treated as satisfied and never polled.
				if dep_service
					.healthcheck
					.as_ref()
					.is_some_and(|h| h.is_disabled())
				{
					continue;
				}
				let container = self.first_replica_name(&dep, dep_service);
				map.entry(container.clone()).or_insert_with(|| {
					let c = container.clone();
					async move {
						self.wait_healthy(&c, dep_service, None)
							.await
							.map_err(Arc::new)
					}
					.boxed()
					.shared()
				});
			}
		}
		map
	}
}

/// Rebuild an owned error from a shared readiness failure, preserving the
/// variant a caller matches on.
///
/// Sharing one poller across dependents forces its error behind an `Arc`
/// ([`SharedReady`]), and `ComposeError` is not `Clone`. Wrapping that `Arc` in
/// [`ComposeError::DependencyNotReady`] for every failure changes what `up()`
/// returns: code matching `ComposeError::HealthCheckTimeout(_)` stops matching
/// once the poller is shared, even though the message and the exit code are
/// identical: an invisible break of a frozen public API.
///
/// `wait_healthy` fails exactly three ways. Two carry cheap owned data and are
/// reconstructed exactly, so a caller sees the variant it saw before the poller
/// was shared. A [`ComposeError::Podman`] transport error holds a non-`Clone`
/// payload and cannot be rebuilt, so it keeps the transparent wrapper, which is
/// what [`ComposeError::innermost`] exists to peel.
pub(super) fn unshare_readiness_error(shared: &Arc<ComposeError>) -> ComposeError {
	match &**shared {
		ComposeError::HealthCheckTimeout(container) => {
			ComposeError::HealthCheckTimeout(container.clone())
		}
		ComposeError::WaitServiceExited { container, code } => ComposeError::WaitServiceExited {
			container: container.clone(),
			code: *code,
		},
		_ => ComposeError::DependencyNotReady(Arc::clone(shared)),
	}
}

#[cfg(all(test, unix))]
#[path = "readiness_tests.rs"]
mod tests;
