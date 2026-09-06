//! `ls` discovers podup-managed compose projects on the host.
//!
//! Unlike the other commands this is project-agnostic: it scans every container
//! carrying a `podup.project` label and groups by project, so it needs only a
//! [`Client`], not a full [`Engine`](crate::engine::Engine) bound to one project/compose file.

use std::collections::BTreeMap;

use crate::error::{ComposeError, Result};
use crate::libpod::types::container::ContainerListEntry;
use crate::libpod::{urlencoded, Client, API_PREFIX};

/// Options for [`list_projects`] (`docker compose ls`).
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`LsOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct LsOptions {
	/// Include projects whose containers are all stopped.
	pub all: bool,
	/// Print only project names.
	pub quiet: bool,
	/// Emit a JSON array instead of a table.
	pub json: bool,
}

impl LsOptions {
	/// Every `docker compose ls` flag, in CLI order. A constructor rather than
	/// a struct literal because the type is `#[non_exhaustive]`, so the next
	/// flag to land is not a breaking change for anyone building one.
	pub fn new(all: bool, quiet: bool, json: bool) -> Self {
		Self { all, quiet, json }
	}

	/// Include projects whose containers are all stopped, `-a/--all`.
	/// Builder-style.
	#[must_use]
	pub fn with_all(mut self, all: bool) -> Self {
		self.all = all;
		self
	}

	/// Print only project names, `-q/--quiet`. Builder-style.
	#[must_use]
	pub fn with_quiet(mut self, quiet: bool) -> Self {
		self.quiet = quiet;
		self
	}

	/// Emit a JSON array instead of a table, `--format json`. Builder-style.
	#[must_use]
	pub fn with_json(mut self, json: bool) -> Self {
		self.json = json;
		self
	}
}

/// Split `ls --filter KEY=VALUE` predicates into name, status, and unknown
/// buckets. Pure so it is unit-tested.
fn split_ls_filters(filters: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
	let (mut names, mut status, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
	for f in filters {
		match f.split_once('=') {
			Some(("name", v)) => names.push(v.to_string()),
			Some(("status", v)) => status.push(v.to_ascii_lowercase()),
			_ => unknown.push(f.clone()),
		}
	}
	(names, status, unknown)
}

/// Whether a project row passes the parsed name/status filters. `running` is the
/// project's roll-up running flag. Pure so it is unit-tested.
fn ls_row_matches(name: &str, running: bool, names: &[String], status: &[String]) -> bool {
	let name_ok = names.is_empty() || names.iter().any(|n| name.contains(n.as_str()));
	let status_word = if running { "running" } else { "exited" };
	let status_ok = status.is_empty() || status.iter().any(|s| s == status_word);
	name_ok && status_ok
}

/// Whether a libpod `Status` string denotes a running container. Podman reports
/// `"running"` (or a human `"Up …"`) for live containers and `"exited"`/`"Exited
/// …"`/`"created"` otherwise. Pure so it can be unit-tested.
fn is_running(status: &str) -> bool {
	let s = status.trim();
	s.eq_ignore_ascii_case("running") || s.to_ascii_lowercase().starts_with("up")
}

/// Whether a libpod `Status`/`State` string denotes a paused container. Podman
/// reports `"paused"` for the machine state and `"Paused"` in the human status.
/// Pure so it can be unit-tested. `docker compose ls` surfaces this state rather
/// than hiding the project or mislabelling it as exited.
fn is_paused(status: &str) -> bool {
	status.trim().to_ascii_lowercase().starts_with("paus")
}

/// A project's roll-up: running, paused, and total replica counts. Stopped
/// replicas are the remainder (`total - running - paused`).
#[derive(Default)]
struct Tally {
	running: usize,
	paused: usize,
	total: usize,
	/// The `podup.config-files` label, from the first container that carries one.
	/// Empty when no container in the project has it: created before the label
	/// existed, or by an embedder that supplied no paths.
	config_files: String,
}

/// List podup projects on the host (`docker compose ls`). Groups every
/// `podup.project`-labelled container by project; by default shows only
/// projects with at least one running container (`all` includes stopped ones).
/// For the `--filter name=/status=` predicates use [`list_projects_filtered`].
pub async fn list_projects(client: &Client, opts: LsOptions) -> Result<()> {
	list_projects_filtered(client, opts, &[]).await
}

/// List podup projects (`docker compose ls`) narrowed by `--filter` predicates
/// (`name=<NAME>`, `status=<running|exited>`). The `filters` slice is kept off
/// the frozen [`LsOptions`] struct so the published library API stays stable across minors.
pub async fn list_projects_filtered(
	client: &Client,
	opts: LsOptions,
	filters: &[String],
) -> Result<()> {
	let label_filters = serde_json::json!({ "label": ["podup.project"] });
	let path = format!(
		"{API_PREFIX}/containers/json?all=true&filters={}",
		urlencoded(&label_filters.to_string()),
	);
	let containers = client
		.get_json::<Vec<ContainerListEntry>>(&path)
		.await
		.map_err(ComposeError::Podman)?;

	// Group by the project label, in name order for deterministic output.
	let mut projects: BTreeMap<String, Tally> = BTreeMap::new();
	for c in &containers {
		let Some(project) = c.labels.get("podup.project") else {
			continue;
		};
		let tally = projects.entry(project.clone()).or_insert(Tally {
			running: 0,
			paused: 0,
			total: 0,
			config_files: String::new(),
		});
		tally.total += 1;
		// Take the first container that carries the label. Every container in a
		// project is created from the same file set, so they agree; taking the
		// first also means a project with one pre-label container still reports
		// the path once any container is recreated.
		if tally.config_files.is_empty() {
			if let Some(files) = c.labels.get("podup.config-files") {
				tally.config_files = files.clone();
			}
		}
		// Podman's libpod list leaves `Status` empty and uses `State`; accept
		// either so the roll-up is robust across response shapes. A paused
		// container is counted separately so it is neither hidden nor mislabelled
		// as exited.
		if is_running(&c.state) || is_running(&c.status) {
			tally.running += 1;
		} else if is_paused(&c.state) || is_paused(&c.status) {
			tally.paused += 1;
		}
	}

	// An unsupported key is an error, not a warning; see the note on the ps
	// filters: silently answering a different question is worse than refusing.
	let (name_filter, status_filter, unknown) = split_ls_filters(filters);
	if let Some(u) = unknown.first() {
		return Err(ComposeError::Unsupported(format!(
			"unsupported ls filter {u:?}: expected name=<NAME> or status=<running|exited>"
		)));
	}
	// A project is "active" (shown without `--all`) when any replica is running
	// or paused; only all-stopped projects are hidden by default. The `--filter`
	// name=/status= predicates further narrow the shown rows.
	let rows: Vec<(&String, &Tally)> = projects
		.iter()
		.filter(|(_, t)| opts.all || t.running > 0 || t.paused > 0)
		.filter(|(name, t)| ls_row_matches(name, t.running > 0, &name_filter, &status_filter))
		.collect();

	if opts.quiet {
		for (name, _) in &rows {
			println!("{name}");
		}
		return Ok(());
	}

	if opts.json {
		let arr: Vec<_> = rows
			.iter()
			.map(|(name, t)| project_row(name, t, &t.config_files))
			.collect();
		println!("{}", super::to_pretty_json("ls.row", &arr)?);
		return Ok(());
	}

	// `ls` was the only list whose first column carried no identity colour, so
	// the one command whose whole job is to name projects rendered them all in
	// the same plain text. Each project now gets its own stable colour, which is
	// what makes a long list scannable.
	//
	// Not the same colour as that project's containers, and it cannot be: an
	// identity colour is keyed on the label with the project prefix stripped, so
	// `proj-web-1` keys on `web` while the row here keys on `proj`. What is
	// promised is per-project stability, not cross-command agreement with the
	// services inside it.
	let mut table = crate::ui::Table::new(&["NAME", "STATUS"])
		.cap(0, 48)
		.identity_col(0)
		.status_col(1);
	for (name, t) in &rows {
		table.push(vec![name.to_string(), status_label(t)]);
	}
	if table.is_empty() {
		// An empty ls table is a legitimate answer; print the explicit
		// "no projects" line on stderr so a script capturing stdout (or
		// `--format json`) sees nothing (#1675).
		crate::ui::progress_note("no projects");
		return Ok(());
	}
	table.print();
	Ok(())
}

/// Per-state replica counts joined as `running(2), paused(1), exited(1)`,
/// mirrors the `docker compose ls` status column, which surfaces each state
/// rather than collapsing to a single running count and discarding the rest.
fn status_label(t: &Tally) -> String {
	let exited = t.total.saturating_sub(t.running).saturating_sub(t.paused);
	let mut parts = Vec::new();
	if t.running > 0 {
		parts.push(format!("running({})", t.running));
	}
	if t.paused > 0 {
		parts.push(format!("paused({})", t.paused));
	}
	if exited > 0 {
		parts.push(format!("exited({exited})"));
	}
	if parts.is_empty() {
		// No replicas at all (an edge case); report a zero exited count.
		parts.push(format!("exited({})", t.total));
	}
	parts.join(", ")
}

/// One `ls --format json` row, matching `docker compose ls --format json`.
///
/// `ConfigFiles` comes from the `podup.config-files` label the containers carry
/// Projects are discovered by label and there is no other record of where a
/// compose file lives. It is empty for a container created before that label
/// existed, or by an embedder that did not supply the paths.
fn project_row(name: &str, t: &Tally, config_files: &str) -> serde_json::Value {
	serde_json::json!({
		"Name": name,
		"Status": status_label(t),
		"ConfigFiles": config_files,
	})
}

#[cfg(test)]
#[path = "projects_tests.rs"]
mod tests;
