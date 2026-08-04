//! Deciding whether an operation whose response was dropped actually landed.
//!
//! The transport cannot answer it. A libpod call that is severed before its
//! response completes looks identical whether the operation ran or not (#1104),
//! and on Podman 6 under concurrency that happens on exactly the state-changing
//! calls — `exec`, `restart`, `stop`, container `DELETE` — after a slow one
//! (#1339). It is not a client deadline and not a pooled-connection race; both
//! were ruled out by measurement.
//!
//! So it is answered the way `cp` and `stats` already answer theirs: by asking
//! the observable the transport cannot see.

use crate::error::{ComposeError, Result};

use crate::engine::Engine;
use crate::libpod::API_PREFIX;

/// What a lifecycle operation was trying to achieve.
///
/// The transport cannot say whether a dropped response means the operation
/// failed or completed and lost only its reply — the two are indistinguishable
/// at HTTP (#1104). This names the observable that answers it out of band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LifecycleGoal {
	/// `start`, `restart` — the container should be running afterwards.
	Running,
	/// `kill`, `stop` — the container should not be running afterwards.
	NotRunning,
	/// `rm` — the container should not exist afterwards. Distinct from
	/// [`Self::NotRunning`]: a stopped-but-present container satisfies that one
	/// and would read a failed removal as a success.
	Gone,
}

impl LifecycleGoal {
	/// Whether libpod's `State` satisfies this goal. `None` means the container
	/// no longer exists — which reaches `NotRunning` and `Gone`, and fails
	/// `Running`.
	pub(super) fn reached(self, state: Option<&str>) -> bool {
		match self {
			Self::Running => state == Some("running"),
			Self::NotRunning => state != Some("running"),
			Self::Gone => state.is_none(),
		}
	}
}

impl Engine {
	/// Decide whether an operation whose response was dropped actually landed.
	///
	/// Shared by every state-changing call that can lose its reply, so they
	/// cannot drift into different answers to the same question — the way the
	/// lane's retry list and its flake counter drifted apart in #1104.
	///
	/// Success requires the container to have reached `goal`. Both other shapes
	/// fail closed: a container that did not reach it, and a re-check that could
	/// not be read. Neither is confirmation, and reporting success without one is
	/// the failure this exists to prevent.
	pub(super) async fn confirm_lost_response(
		&self,
		container: &str,
		done: &str,
		goal: LifecycleGoal,
		e: crate::libpod::PodmanError,
	) -> Result<bool> {
		match self.container_state(container).await {
			Ok(state) if goal.reached(state.as_deref()) => {
				tracing::warn!(
					"{container}: {done} lost its response [{}] but the container reached \
					 {goal:?}, so the operation landed",
					e.stream_end_kind()
				);
				crate::ui::progress_line("Container", container, done);
				Ok(true)
			}
			Ok(state) => {
				tracing::warn!(
					"{container}: {done} lost its response [{}] and the container is \
					 {state:?}, not {goal:?}",
					e.stream_end_kind()
				);
				Err(ComposeError::Podman(e))
			}
			Err(recheck) => {
				tracing::warn!(
					"{container}: {done} lost its response [{}] and the state could not be \
					 re-checked: {recheck}",
					e.stream_end_kind()
				);
				Err(ComposeError::Podman(e))
			}
		}
	}

	/// libpod's `State` for one container, or `None` when it no longer exists.
	///
	/// Listed rather than inspected: the list carries `State` directly and the
	/// inspect response is several times the size for the one field wanted here
	/// (#1298). libpod's `name` filter matches on substring, so the exact name is
	/// picked out of the results rather than trusted from the query.
	pub(super) async fn container_state(&self, container: &str) -> Result<Option<String>> {
		let filters = serde_json::json!({ "name": [container] });
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			crate::libpod::urlencoded(&filters.to_string()),
		);
		let entries = self
			.client
			.get_json::<Vec<crate::libpod::types::container::ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;
		Ok(entries
			.into_iter()
			.find(|e| {
				e.names
					.iter()
					.any(|n| n.trim_start_matches('/') == container)
			})
			.map(|e| e.state))
	}
}
