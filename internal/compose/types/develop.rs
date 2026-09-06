//! Development watch configuration types for the `develop:` service key.
//!
//! [`DevelopConfig`] holds a list of [`WatchRule`]s that drive the file-watch
//! engine. Each rule specifies a host path to monitor, an [`WatchAction`] to
//! take on change (sync, rebuild, restart, or sync+exec), and optional
//! ignore/include glob filters.

use serde::{Deserialize, Serialize};

/// `develop:` service key: holds file-watch rules for the `watch` command.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DevelopConfig {
	/// The rules in declaration order. `watch` evaluates them in order and the
	/// first matching path wins, so order is meaningful rather than cosmetic.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub watch: Vec<WatchRule>,
}

/// A single `develop.watch` rule: a path to watch, an action to take, and optional ignore filters.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct WatchRule {
	/// Host path watched for changes, resolved relative to the project base directory.
	pub path: String,
	/// What to do when a watched file changes.
	pub action: WatchAction,
	/// Sync destination inside the container; sync actions are skipped when absent.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub target: Option<String>,
	/// Glob patterns (relative to the base dir) whose matches are excluded from triggering.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub ignore: Vec<String>,
	/// Glob allow-list; when non-empty, only matching paths trigger the rule.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub include: Vec<String>,
	/// When true, copy `path` to `target` once at startup before watching begins.
	#[serde(default)]
	pub initial_sync: bool,
	/// Command run inside the container for the `sync+exec` action.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub exec: Option<WatchExec>,
	/// Keys under this rule that podup does not recognise, kept so `config`
	/// round-trips a compose file without silently dropping them. Insertion
	/// order is preserved.
	#[serde(flatten, default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
	pub unknown: indexmap::IndexMap<String, serde_yaml::Value>,
}

/// Action triggered by a `develop.watch` rule: `sync`, `rebuild`, `restart`, `sync+restart`, or `sync+exec`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum WatchAction {
	/// `sync`: copy changed files from `path` into the container at `target`.
	#[default]
	Sync,
	/// `rebuild`: rebuild the service image and recreate the container.
	Rebuild,
	/// `restart`: restart the running container without rebuilding.
	Restart,
	/// `sync+restart`: sync changed files, then restart the container.
	SyncAndRestart,
	/// `sync+exec`: sync changed files, then run the rule's `exec` command in the container.
	SyncAndExec,
}

impl WatchAction {
	/// The lowercase compose token for this action (the inverse of the custom
	/// [`Deserialize`]). Kept in sync with the parser so `config` output
	/// round-trips back through podup.
	pub fn as_token(&self) -> &'static str {
		match self {
			WatchAction::Sync => "sync",
			WatchAction::Rebuild => "rebuild",
			WatchAction::Restart => "restart",
			WatchAction::SyncAndRestart => "sync+restart",
			WatchAction::SyncAndExec => "sync+exec",
		}
	}

	/// True for actions whose semantics require a `target` (the sync family).
	/// `rebuild`/`restart` operate on the whole container and need no target.
	pub fn requires_target(&self) -> bool {
		matches!(
			self,
			WatchAction::Sync | WatchAction::SyncAndRestart | WatchAction::SyncAndExec
		)
	}
}

impl Serialize for WatchAction {
	fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
		// Emit the lowercase compose token (not the PascalCase variant name) so
		// `config` output is re-ingestible by the custom `Deserialize` below.
		s.serialize_str(self.as_token())
	}
}

impl<'de> Deserialize<'de> for WatchAction {
	fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		let s = String::deserialize(d)?;
		match s.as_str() {
			"sync" => Ok(WatchAction::Sync),
			"rebuild" => Ok(WatchAction::Rebuild),
			"restart" => Ok(WatchAction::Restart),
			"sync+restart" => Ok(WatchAction::SyncAndRestart),
			"sync+exec" => Ok(WatchAction::SyncAndExec),
			other => Err(serde::de::Error::custom(format!(
				"unknown watch action: {other}"
			))),
		}
	}
}

/// Exec command run as part of a `sync+exec` watch action.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WatchExec {
	/// Command and arguments executed in the container (argv form, not shell-parsed).
	pub command: Vec<String>,
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "develop_tests.rs"]
mod tests;
