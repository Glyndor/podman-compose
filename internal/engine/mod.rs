//! Container orchestration engine.
//!
//! Translates a parsed [`ComposeFile`](crate::compose::types::ComposeFile) into Podman API calls via the libpod REST API.

mod build;
mod container;
/// `pub(crate)` so the fuzz harness behind the `test-helpers` feature can
/// reach `archive::extract_tar_guarded` without widening the published API:
/// the chain stays `engine → copy → archive` all `pub(crate)` (or stricter),
/// which a third-party crate cannot see.
pub(crate) mod copy;
pub(crate) mod events;
mod terminal_pump;
pub use events::EventsOptions;
mod image;
pub use build::{BuildOptions, PullOptions, PushOptions};
pub use copy::CpOptions;
pub use image::{resolve_image_digests, CommitOptions};
pub use lifecycle::{validate_stop_timeout, RunOptions, RunOverrides};
pub use lock::ProjectLock;
mod interactive;
pub(crate) use interactive::wants_interactive_run;
mod project_label;
use project_label::{build_project_label_parts, ProjectLabelParts};
mod replicas;
use replicas::resolve_replica_name;

mod container_config;
pub(crate) use container_config::build_log_config;
#[cfg(test)]
mod fake_podman;
mod health;
mod lifecycle;
mod lock;
mod names;
mod network;
/// Re-exported so `startup::config_render` (in the binary crate) can call the
/// same function `up` uses, instead of duplicating the resolution rule.
pub use network::resolve_network_name;
mod pod;
mod profiles;
pub use profiles::{retain_active_profiles, retain_active_profiles_with_targets};
mod projects;
pub use projects::{list_projects, list_projects_filtered, LsOptions};
pub(crate) mod query;
pub use query::{
	AttachOptions, AttachOutcome, AttachSummary, ExecOptions, ImagesOptions, LogsDisplay,
	LogsOptions, PsDisplayOptions, PsFilterOptions, PsOptions, DEFAULT_LOG_TAIL,
};
mod secrets;
mod staging;
mod stats;
pub use staging::is_safe_project_name;
pub use stats::StatsOptions;
mod volume;
pub use volume::{VolumesDisplayOptions, VolumesOptions};
mod volume_mounts;
mod walk;
#[cfg(feature = "watch")]
mod watch;

use std::io::Write;
use std::path::PathBuf;

use futures_util::StreamExt;

use crate::compose::types::{LifecycleHook, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::exec::{ExecCreateConfig, ExecStartConfig};
use crate::libpod::{Client, LogOutput, API_PREFIX};

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// Handle through which all Podman operations for a project are dispatched.
pub struct Engine {
	pub(super) client: Client,
	pub(super) project: String,
	pub(super) base_dir: PathBuf,
	/// Absolute compose-file paths this engine was built from, in `-f` order.
	/// Stamped onto every container as `podup.config-files` so `ls` can report
	/// them: projects are discovered by label, with no other record of where
	/// their compose file lives. Empty when the caller did not supply them, in
	/// which case the label is omitted rather than written blank.
	pub(super) compose_files: Vec<PathBuf>,
	/// Optional CLI `-t/--timeout` override (seconds) for container shutdown
	/// grace; when set it takes precedence over each service's
	/// `stop_grace_period`. `None` falls back to the per-service value.
	pub(super) stop_timeout: Option<i32>,
	/// CLI `--scale SERVICE=N` overrides (from `up --scale` and the `scale`
	/// subcommand); when a service is present it takes precedence over the
	/// compose `scale:`/`deploy.replicas` value. Empty falls back to compose.
	pub(super) scale_overrides: std::collections::HashMap<String, u32>,
	/// CLI `up --pull <policy>` override; takes precedence over each service's
	/// `pull_policy`. `None` falls back to the per-service value.
	pub(super) pull_policy_override: Option<String>,
	/// CLI `up --no-build`: never build images, even for services with a
	/// `build:` section (they fall back to pulling/using an existing image).
	pub(super) no_build: bool,
	/// CLI `up --quiet-pull`: suppress image-pull progress output.
	pub(super) quiet_pull: bool,
	/// CLI `run`-only flag overrides (user/workdir/entrypoint/volume/publish/
	/// interactive/no-deps); empty by default.
	pub(super) run_overrides: lifecycle::RunOverrides,
	/// Global `--env-file` paths that double as `docker compose run --env-file`:
	/// their contents seed a one-off `run` container's environment at the lowest
	/// precedence (env-file < service `environment:` < `-e`). Resolved relative
	/// to `base_dir`; empty by default. Kept off the frozen public
	/// [`lifecycle::RunOverrides`] struct so the library API stays stable.
	pub(super) run_env_files: Vec<String>,
	/// CLI `docker compose run -l/--label KEY=VAL` ad-hoc labels for the one-off
	/// `run` container; empty by default. Kept off the frozen public
	/// [`lifecycle::RunOverrides`] struct so the library API stays stable.
	pub(super) run_labels: Vec<String>,
	/// CLI `run -T/--no-TTY`: opt out of the pseudo-TTY.
	pub(super) run_no_tty: bool,
	/// CLI `up -V/--renew-anon-volumes`: when recreating a container, also remove
	/// its old anonymous volumes instead of leaving them orphaned.
	pub(super) renew_anon_volumes: bool,
	/// Image references this engine observed present on the host during the
	/// current invocation, recorded by the prefetch stage so the per-service
	/// pull site does not ask again.
	///
	/// Only ever a record of what this engine saw itself, this run: it is not
	/// persisted and not shared between engines. Two engines against different
	/// sockets must not pool observations — a process-wide cache would let one
	/// skip a pull for an image that only exists on the other's host, which
	/// matters because podup is consumed as a library and a caller may hold more
	/// than one engine.
	pub(super) images_seen_present: std::sync::Mutex<std::collections::HashSet<String>>,
	/// CLI `--no-warn`: suppress the host-binding / privilege-escalation
	/// warnings the engine emits during `up`/`create`/`run`/`exec`. The
	/// operator wrote the compose file deliberately, so the default-warning
	/// behaviour is opt-out per run rather than per command. `config`'s
	/// surface-mode listing is unaffected — `config` is the "show me what will
	/// happen" command, where the warnings stay visible there.
	pub(super) no_warn: bool,
	/// The pre-URL-encoded libpod `filters` JSON object that scopes a
	/// container-list call to this project's `podup.project={name}` label,
	/// plus the raw `podup.project={name}` label string. Built once per
	/// [`Engine`] so call sites do not pay `format!` + `serde_json::to_string`
	/// + `urlencoded` on every invocation (#1364).
	project_label: ProjectLabelParts,
}

impl Engine {
	/// Create an engine for `project_name` using the working directory as the
	/// base path for relative volume mounts.
	///
	/// If the working directory cannot be resolved (the process's CWD was
	/// deleted or is unreadable at construction time), the engine falls back
	/// to an empty `base_dir` and a warning is logged. Callers that need a
	/// definite base directory — the CLI does — should use
	/// [`Engine::with_base_dir`] instead, which surfaces a missing or
	/// unreadable directory as a hard error rather than a silent empty
	/// path that later surfaces as a confusing "compose file not found".
	pub fn new(client: Client, project: String) -> Self {
		let base_dir = match std::env::current_dir() {
			Ok(dir) => dir,
			Err(e) => {
				tracing::warn!(
					"cannot resolve the current working directory ({e}); base_dir is empty \
					 and relative paths (compose files, volume mounts, env_file sources) \
					 will not resolve. Use Engine::with_base_dir to set an explicit base \
					 directory."
				);
				PathBuf::new()
			}
		};
		Self {
			client,
			project: project.clone(),
			base_dir,
			compose_files: Vec::new(),
			stop_timeout: None,
			scale_overrides: std::collections::HashMap::new(),
			pull_policy_override: None,
			no_build: false,
			quiet_pull: false,
			run_overrides: lifecycle::RunOverrides::default(),
			run_env_files: Vec::new(),
			run_labels: Vec::new(),
			run_no_tty: false,
			renew_anon_volumes: false,
			images_seen_present: std::sync::Mutex::new(std::collections::HashSet::new()),
			no_warn: false,
			project_label: build_project_label_parts(&project),
		}
	}

	/// Create an engine with an explicit base directory — use when the compose file is not in the working directory.
	pub fn with_base_dir(client: Client, project: String, base_dir: PathBuf) -> Self {
		Self {
			client,
			project: project.clone(),
			base_dir,
			compose_files: Vec::new(),
			stop_timeout: None,
			scale_overrides: std::collections::HashMap::new(),
			pull_policy_override: None,
			no_build: false,
			quiet_pull: false,
			run_overrides: lifecycle::RunOverrides::default(),
			run_env_files: Vec::new(),
			run_labels: Vec::new(),
			run_no_tty: false,
			renew_anon_volumes: false,
			images_seen_present: std::sync::Mutex::new(std::collections::HashSet::new()),
			no_warn: false,
			project_label: build_project_label_parts(&project),
		}
	}

	/// Set the CLI `-t/--timeout` shutdown-grace override (seconds). Builder-style.
	pub fn with_stop_timeout(mut self, timeout: Option<i32>) -> Self {
		self.stop_timeout = timeout;
		self
	}

	/// Set the CLI `--scale SERVICE=N` replica overrides. Builder-style.
	pub fn with_scale_overrides(
		mut self,
		overrides: std::collections::HashMap<String, u32>,
	) -> Self {
		self.scale_overrides = overrides;
		self
	}

	/// Set the CLI `up` image-acquisition overrides: `--pull <policy>`,
	/// `--no-build`, and `--quiet-pull`. Builder-style.
	pub fn with_up_overrides(
		mut self,
		pull_policy: Option<String>,
		no_build: bool,
		quiet_pull: bool,
	) -> Self {
		self.pull_policy_override = pull_policy;
		self.no_build = no_build;
		self.quiet_pull = quiet_pull;
		self
	}

	/// Record the compose files this engine was built from, so containers it
	/// creates carry them as a `podup.config-files` label and `ls` can report
	/// where a project's compose file lives. Builder-style; additive, so an
	/// embedder that does not call it simply gets no label.
	pub fn with_compose_files(mut self, files: Vec<PathBuf>) -> Self {
		self.compose_files = files;
		self
	}

	/// Set the CLI `run`-only flag overrides (`-u/-w/--entrypoint/-v/-p/-i/
	/// --no-deps`). Builder-style; consumed by [`Engine::run`].
	pub fn with_run_overrides(mut self, overrides: RunOverrides) -> Self {
		self.run_overrides = overrides;
		self
	}

	/// Set the global `--env-file` paths that also seed a one-off `run`
	/// container's environment (`docker compose run --env-file`: env-file <
	/// service `environment:` < `-e`). Builder-style; consumed by
	/// [`Engine::run`]. Resolved relative to the engine's base dir.
	pub fn with_run_env_files(mut self, env_files: Vec<String>) -> Self {
		self.run_env_files = env_files;
		self
	}

	/// Set the CLI `docker compose run -l/--label KEY=VAL` ad-hoc labels for the
	/// one-off `run` container. Builder-style; consumed by [`Engine::run`].
	pub fn with_run_labels(mut self, labels: Vec<String>) -> Self {
		self.run_labels = labels;
		self
	}

	/// Set the CLI `run -T/--no-TTY` flag. Builder-style.
	///
	/// Carried here rather than on [`RunOverrides`] for the reason that struct
	/// already documents: it is public and not `#[non_exhaustive]`, so a new
	/// field is a breaking change. `run_env_files` and `run_labels` are on the
	/// engine for exactly this; cargo-semver-checks flagged this field when it
	/// was placed on `RunOverrides`.
	pub fn with_run_no_tty(mut self, no_tty: bool) -> Self {
		self.run_no_tty = no_tty;
		self
	}

	/// Set the CLI `up -V/--renew-anon-volumes` flag. Builder-style; when set,
	/// recreating a container also removes its old anonymous volumes.
	pub fn with_renew_anon_volumes(mut self, renew: bool) -> Self {
		self.renew_anon_volumes = renew;
		self
	}

	/// Set the CLI `--no-warn` flag. Builder-style; when set, the engine
	/// suppresses the host-binding / privilege-escalation warnings it emits
	/// during `up`/`create`/`run`/`exec`. Operators who deliberately wrote
	/// `privileged: true` (or any other host-binding mode) into the compose
	/// file use this to silence the per-run warning; `config`'s surface-mode
	/// listing is unaffected (`config` is the "show me what will happen"
	/// command, where the warning is the whole point).
	pub fn with_no_warn(mut self, no_warn: bool) -> Self {
		self.no_warn = no_warn;
		self
	}

	/// Resolve the replica count for a service: a CLI `--scale` override wins,
	/// else the compose `scale:`, else `deploy.replicas`, else 1. The single
	/// source of truth so `up`, naming, and teardown never drift.
	pub(super) fn resolve_replicas(&self, service_name: &str, service: &Service) -> usize {
		if let Some(&n) = self.scale_overrides.get(service_name) {
			return n as usize;
		}
		declared_replicas(service)
	}

	/// The cached URL-encoded `{"label":["podup.project=…"]}` JSON for the
	/// container-list call sites that scope by the project label only
	/// (#1364).
	pub(super) fn project_label_filter_encoded(&self) -> &str {
		&self.project_label.encoded
	}

	/// The cached URL-encoded `{"label":["podup.project=…"]}` JSON for
	/// network-list call sites. Identical to the container version because
	/// libpod's network and container label filters take the same shape
	/// (#1364).
	pub(super) fn project_network_filter_encoded(&self) -> &str {
		&self.project_label.network_encoded
	}

	/// The cached raw `podup.project={name}` label string, exposed for the
	/// dynamic sites (those that combine the project label with a second
	/// predicate like `podup.service={svc}`) so they can splice the project
	/// half into their own JSON without reformatting it on every call
	/// (#1364).
	pub(super) fn project_label_raw(&self) -> &str {
		&self.project_label.raw
	}

	/// Build the URL-encoded `{"label":[…]}` filter for a project container
	/// call that needs one extra label predicate (typically `podup.service=`).
	/// `extras` carries the additional labels verbatim; the project label is
	/// spliced in once per call so the only `format!` is on the caller's
	/// extras (#1364).
	pub(super) fn project_label_filter_with(
		&self,
		extras: impl IntoIterator<Item = String>,
	) -> String {
		let mut labels: Vec<String> = Vec::new();
		labels.push(self.project_label.raw.clone());
		labels.extend(extras);
		let filter = serde_json::json!({ "label": labels });
		crate::libpod::urlencoded(&filter.to_string())
	}

	pub(super) async fn run_lifecycle_hook(
		&self,
		container_name: &str,
		hook: &LifecycleHook,
	) -> Result<()> {
		let cmd = hook.command.to_exec();
		let env: Vec<String> = {
			let m = hook.environment.to_map();
			m.into_iter()
				.filter_map(|(k, v)| v.map(|v| format!("{k}={v}")))
				.collect()
		};

		let exec_cfg = ExecCreateConfig {
			cmd: Some(cmd),
			user: hook.user.clone(),
			privileged: hook.privileged,
			working_dir: hook.working_dir.clone(),
			env: if env.is_empty() { None } else { Some(env) },
			attach_stdout: Some(true),
			attach_stderr: Some(true),
			..Default::default()
		};

		let path = format!(
			"{API_PREFIX}/containers/{}/exec",
			crate::libpod::urlencoded(container_name)
		);
		let resp: crate::libpod::types::exec::ExecCreateResponse = self
			.client
			.post_json(&path, &exec_cfg)
			.await
			.map_err(ComposeError::Podman)?;
		let exec_id = resp.id;

		let start_cfg = ExecStartConfig {
			detach: false,
			tty: false,
		};
		let start_path = format!(
			"{API_PREFIX}/exec/{}/start",
			crate::libpod::urlencoded(&exec_id)
		);
		let resp = self
			.client
			.post_json_stream(&start_path, &start_cfg)
			.await
			.map_err(ComposeError::Podman)?;

		let mut stream = crate::libpod::parse_multiplexed(resp.into_body());
		// Lock both stdout and stderr per frame, symmetric with the rest of
		// the engine: holding either lock across the await loop would stall any
		// concurrent log emission, including the tracing subscriber that
		// writes to stderr. The previous code held stdout's lock for the
		// whole stream, which serialised the hook behind any other writer for
		// its entire lifetime (#1369). Flush after each frame so output
		// stays prompt.
		while let Some(msg) = stream.next().await {
			match msg.map_err(ComposeError::Podman)? {
				LogOutput::StdOut { message } => {
					let mut out = std::io::stdout().lock();
					let _ = write_frame(&mut out, &message);
					let _ = out.flush();
				}
				LogOutput::StdErr { message } => {
					let mut err = std::io::stderr().lock();
					let _ = write_frame(&mut err, &message);
					let _ = err.flush();
				}
			}
		}

		// A hook that exits non-zero must surface as an error (matching
		// `Engine::run`): otherwise a failing `post_start` readiness/init step is
		// silently treated as success and dependents start against a container
		// that never initialised. `pre_stop` callers deliberately ignore the Err.
		let inspect_path = format!(
			"{API_PREFIX}/exec/{}/json",
			crate::libpod::urlencoded(&exec_id)
		);
		let inspect: crate::libpod::types::exec::ExecInspect = self
			.client
			.get_json(&inspect_path)
			.await
			.map_err(ComposeError::Podman)?;
		if let Some(code) = inspect.exit_code {
			if code != 0 {
				return Err(ComposeError::Build(format!(
					"lifecycle hook exited with status {code}"
				)));
			}
		}

		Ok(())
	}

	pub(super) fn container_name(&self, service_name: &str, service: &Service) -> String {
		service
			.container_name
			.clone()
			.unwrap_or_else(|| format!("{}-{}", self.project, service_name))
	}

	/// Container names for `service` at exactly `count` replicas.
	///
	/// The auto-generated name is **always** index-suffixed (`{project}-{svc}-1`
	/// even for a single replica), matching docker-compose and podman-compose,
	/// which never expose a bare, unnumbered container name. An explicit
	/// `container_name:` is honoured verbatim at a single replica (and
	/// `{name}-1..-N` only when forced past one), since the user asked for that
	/// exact name. A `count` of 0 (`--scale svc=0`) yields no names.
	pub(super) fn replica_names_for(
		&self,
		service_name: &str,
		service: &Service,
		count: usize,
	) -> Vec<String> {
		match &service.container_name {
			Some(explicit) if count <= 1 => {
				if count == 0 {
					Vec::new()
				} else {
					vec![explicit.clone()]
				}
			}
			Some(explicit) => (1..=count).map(|i| format!("{explicit}-{i}")).collect(),
			None => {
				let base = format!("{}-{}", self.project, service_name);
				(1..=count).map(|i| format!("{base}-{i}")).collect()
			}
		}
	}

	pub(super) fn replica_names(&self, service_name: &str, service: &Service) -> Vec<String> {
		let replicas = self.resolve_replicas(service_name, service);
		self.replica_names_for(service_name, service, replicas)
	}

	pub(super) fn first_replica_name(&self, service_name: &str, service: &Service) -> String {
		// Falls back to the bare base only when the service resolves to zero
		// replicas (`--scale svc=0`), so callers that cannot represent "no
		// container" still get a stable, addressable name.
		self.replica_names(service_name, service)
			.into_iter()
			.next()
			.unwrap_or_else(|| self.container_name(service_name, service))
	}

	/// Resolve the container name for a service replica from the statically
	/// derived names: the 1-based `--index` when given (erroring if out of
	/// range), else the first replica.
	///
	/// Prefer [`Engine::live_replica_name_at`] for the replica-targeting
	/// commands (`exec`, `cp`): the static names reflect only the compose
	/// `scale:`/`deploy.replicas` (plus a `--scale` on the *current* invocation),
	/// so a later `cp`/`exec` would not see replicas created by a prior
	/// `up --scale`. This variant stays for callers that cannot await.
	pub(super) fn replica_name_at(
		&self,
		service_name: &str,
		service: &Service,
		index: Option<u32>,
	) -> Result<String> {
		let names = self.replica_names(service_name, service);
		let base = self.container_name(service_name, service);
		resolve_replica_name(service_name, &base, &names, index)
	}

	/// Resolve the container name for a service replica against the *running*
	/// scale: the replicas Podman actually has (matched by the `podup.service`
	/// label), falling back to the statically derived names before anything is
	/// created. `--index n` therefore targets replica `n` even when it was
	/// created by an earlier `up --scale`/`scale` rather than the current
	/// invocation, matching `docker compose cp/exec --index`. Shared by the
	/// replica-targeting commands (`exec`, `cp`).
	///
	/// Reads off the bulk project listing ([`Engine::live_project_replicas_sorted`])
	/// instead of issuing a per-service container-list round-trip (#1445):
	/// one shared GET powers the rest of the per-replica query paths
	/// (`port`, `logs`) when a command needs more than one of them.
	pub(super) async fn live_replica_name_at(
		&self,
		service_name: &str,
		service: &Service,
		index: Option<u32>,
	) -> Result<String> {
		let live_by_service = self.live_project_replicas_sorted().await?;
		let names = match live_by_service.get(service_name) {
			Some(names) if !names.is_empty() => names.clone(),
			// Service has no live container yet — fall back to the static
			// compose names so a never-created service still has an
			// addressable replica.
			_ => self.replica_names(service_name, service),
		};
		let base = self.container_name(service_name, service);
		resolve_replica_name(service_name, &base, &names, index)
	}

	/// Watch for file changes and apply the service's `develop.watch` rules. Returns an error when the `watch` feature is disabled.
	#[cfg(not(feature = "watch"))]
	pub async fn watch(&self, _file: &crate::compose::types::ComposeFile) -> Result<()> {
		Err(crate::error::ComposeError::Unsupported(
			"watch requires the 'watch' feature".into(),
		))
	}
}

// ---------------------------------------------------------------------------
// JSON serialisation helpers
// ---------------------------------------------------------------------------

/// Serialise `v` to a compact JSON string for embedding into a libpod query
/// parameter or NDJSON row.
///
/// Six sites flow through this — five in [`build`](super::build) (`cachefrom`,
/// `buildargs`, `labels`, `secrets`, `cacheto`) and one in
/// [`events`](super::events) (the `--format json` event row). Each of them
/// used to call `serde_json::to_string(...).unwrap_or_default()`, which
/// silently emitted `""` on a serialisation failure; the empty string was
/// then placed verbatim into the query parameter, where libpod treats it as
/// "no value provided", so the user's args were ignored and the build ran
/// with image defaults. The events side dropped the row and corrupted the
/// NDJSON stream a parser was reading line-by-line (#1366).
///
/// `what` names the offending field in the error so the operator sees which
/// value was rejected.
pub(super) fn to_query_json<T: serde::Serialize>(what: &str, v: &T) -> Result<String> {
	serde_json::to_string(v).map_err(|e| ComposeError::Build(format!("invalid {what}: {e}")))
}

/// Serialise `v` to a pretty-printed JSON string for emitting as the final
/// `--format json` output of a list command.
///
/// Five sites flow through this: `ls` ([`projects::list_projects_filtered`]),
/// `images` ([`Engine::images_with_services`]), `top`
/// ([`Engine::top_with_options`]), `ps` ([`Engine::ps_filtered_with_display`]),
/// and `volumes` ([`Engine::list_volumes_with_display`]). Each used to call
/// `serde_json::to_string_pretty(...).unwrap_or_default()`, which silently
/// emitted an empty string on a serialisation failure; the command then
/// printed the empty string and exited 0, so a script consuming
/// `podup <cmd> --format json` received an empty document indistinguishable
/// from "no results" (#1444). Unlike the NDJSON path in
/// [`to_query_json`](self)/[`events`](super::events) — where one row can be
/// dropped and the stream continue — `--format json` is the *whole* output,
/// so a failure must propagate as an error and exit non-zero.
///
/// `what` names the offending field in the error so the operator sees which
/// row or document type was rejected.
pub(super) fn to_pretty_json<T: serde::Serialize>(what: &str, v: &T) -> Result<String> {
	serde_json::to_string_pretty(v).map_err(|e| ComposeError::Build(format!("invalid {what}: {e}")))
}

/// Write one log frame without creating a lossy `Cow` for valid UTF-8.
///
/// `String::from_utf8_lossy` allocates a `Cow<String>` even on the happy path;
/// `std::str::from_utf8` returns `&str` directly with no allocation. The
/// lossy path is the rare one (an arbitrary byte stream from `run`/`exec`/
/// hook output), so a single shared helper keeps the two call sites
/// (`Engine::run` and `run_lifecycle_hook`) honest and matches the previous
/// observable output (#1369).
pub(crate) fn write_frame<W: std::io::Write>(out: &mut W, bytes: &[u8]) -> std::io::Result<()> {
	match std::str::from_utf8(bytes) {
		Ok(text) => out.write_all(text.as_bytes()),
		Err(_) => out.write_all(String::from_utf8_lossy(bytes).as_bytes()),
	}
}

/// Emit one `tracing::warn!` per active host-binding / privilege-escalation mode
/// across every service in `file`.
///
/// The `config` command uses this to surface the active modes at the default
/// log level (CI logs see them even when the operator never runs `up`). It is
/// deliberately not gated on `--no-warn` — `config` is the "show me what will
/// happen" command, where the warning is the whole point. The live
/// `up`/`create`/`run`/`exec` paths emit the same warnings per-call but honour
/// `--no-warn`.
pub fn surface_host_modes(file: &crate::compose::types::ComposeFile) {
	for (name, service) in &file.services {
		for w in self::container::check_host_mode(name, service) {
			tracing::warn!("{}", w.message);
		}
	}
}

// ---------------------------------------------------------------------------
// Replica resolution helpers (see `replicas.rs`)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Filesystem helpers (see `walk.rs`)
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "to_query_json_tests.rs"]
mod to_query_json_tests;

/// How many replicas a service declares in the file, before any `--scale` on
/// the current invocation. Split out of [`Engine::resolve_replicas`] so a
/// caller with no `Engine` (autostart's start mode) reads the same rule rather
/// than restating it.
pub(crate) fn declared_replicas(service: &Service) -> usize {
	service
		.scale
		.or(service.deploy.as_ref().and_then(|d| d.replicas))
		.unwrap_or(1) as usize
}

/// The container name a service resolves to at exactly one replica.
///
/// This is [`Engine::replica_names_for`] at `count == 1`, extracted so
/// autostart's start mode names the container the engine actually created
/// instead of spelling the rule out a second time. The two are pinned together
/// by `naming_agrees_with_the_engine_at_one_replica`; without that test this
/// would be a copy that drifts silently, and the unit would name a container
/// that does not exist.
pub(crate) fn sole_replica_name(project: &str, service_name: &str, service: &Service) -> String {
	match &service.container_name {
		Some(explicit) => explicit.clone(),
		None => format!("{project}-{service_name}-1"),
	}
}

mod start_mode;

#[cfg(test)]
#[path = "to_pretty_json_tests.rs"]
mod to_pretty_json_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stream_end_tests;
#[cfg(unix)]
#[cfg(test)]
mod wait_timeout_tests;
