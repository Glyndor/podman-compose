//! Pure host-path → container placement and watch-event helpers.
//!
//! These functions hold the side-effect-free decisions of the watch engine:
//! mapping a changed host path to its container archive placement, filtering
//! which notify events drive a sync, validating that sync rules carry a target,
//! and bookkeeping for the per-target `mkdir`. Keeping them here lets the
//! dispatch loop in [`super`] stay focused on I/O.

use std::collections::HashSet;
use std::path::Path;

use crate::compose::types::WatchRule;
use crate::error::{ComposeError, Result};

/// Where a changed host path lands inside the container for a `sync` action:
/// the archive entry name and the directory the tar is extracted at.
pub(super) struct SyncPlacement {
	/// Archive path the changed entry occupies inside the tar.
	pub(super) entry_name: String,
	/// Container directory the archive is PUT (extracted) at.
	pub(super) dest_dir: String,
}

/// Map a changed host path to its container archive placement, matching
/// docker-compose `watch` semantics.
///
/// `root` is the watch rule's absolute host path, `changed` the path that
/// actually changed (equal to `root` for a single-file rule, a descendant for a
/// directory rule), and `target` the rule's container target.
///
/// For a directory rule the changed entry keeps its path relative to `root`
/// (subdirectories preserved) and is extracted under `target` treated as a
/// directory. For a single-file rule the entry is stored under
/// `basename(target)` and extracted into `target`'s parent, so a renaming
/// target is honoured.
pub(super) fn plan_sync_placement(root: &Path, changed: &Path, target: &str) -> SyncPlacement {
	if root.is_dir() {
		// Directory rule: preserve the changed file's subpath under `target`,
		// which is treated as a directory.
		let rel = changed.strip_prefix(root).unwrap_or(changed);
		let entry_name = rel.to_string_lossy().into_owned();
		let dest_dir = target.trim_end_matches('/').to_string();
		let dest_dir = if dest_dir.is_empty() {
			"/".to_string()
		} else {
			dest_dir
		};
		SyncPlacement {
			entry_name,
			dest_dir,
		}
	} else {
		// Single-file rule: store under the target basename so a renaming target
		// is honoured, and extract into the target's parent directory.
		let target_path = Path::new(target);
		let entry_name = target_path
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.or_else(|| {
				changed
					.file_name()
					.map(|n| n.to_string_lossy().into_owned())
			})
			.unwrap_or_default();
		let dest_dir = target_path
			.parent()
			.map(|p| p.to_string_lossy().into_owned())
			.filter(|s| !s.is_empty())
			.unwrap_or_else(|| "/".to_string());
		SyncPlacement {
			entry_name,
			dest_dir,
		}
	}
}

/// True when a notify event should drive a watch action.
///
/// docker-compose `watch` only reacts to write/create/remove/rename changes. The
/// vendored notify inotify backend also emits `Access` events (it sets
/// `WatchMask::OPEN`), so merely opening/reading a watched file would otherwise
/// fire a sync — and the sync's own read of the source re-opens the path,
/// generating fresh `Access` events that feed back into another sync. Filtering
/// to create/modify/remove (rename is a `Modify(Name(..))`) matches compose
/// semantics and breaks that feedback loop.
pub(super) fn is_dispatch_event(kind: &notify::EventKind) -> bool {
	use notify::EventKind;
	matches!(
		kind,
		EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
	)
}

/// Reject a watch rule whose action needs a `target` but has none. docker
/// compose treats a sync rule without a target as a configuration error rather
/// than silently performing no sync.
pub(super) fn validate_sync_target(rule: &WatchRule) -> Result<()> {
	if rule.action.requires_target() && rule.target.is_none() {
		return Err(ComposeError::Watch(format!(
			"watch rule for '{}' uses a sync action ({}) but has no target",
			rule.path,
			rule.action.as_token()
		)));
	}
	Ok(())
}

/// Best-effort `mkdir -p` argv for creating a sync target directory. The `--`
/// terminates options so a target beginning with `-` (e.g. `-m0777`) is treated
/// as a path, not parsed as a flag by busybox `mkdir`.
pub(super) fn mkdir_p_argv(dest_dir: &str) -> Vec<String> {
	vec![
		"mkdir".into(),
		"-p".into(),
		"--".into(),
		dest_dir.to_string(),
	]
}

/// Record that `(container, dest)` has had its directory ensured, returning
/// `true` the first time (the caller should then issue the `mkdir`) and `false`
/// thereafter so the per-event `mkdir` exec is issued at most once per target.
pub(super) fn mark_dir_ensured(
	ensured: &mut HashSet<(String, String)>,
	container: &str,
	dest: &str,
) -> bool {
	ensured.insert((container.to_string(), dest.to_string()))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "placement_tests.rs"]
mod tests;
