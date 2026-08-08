//! Container orchestration engine.
//!
//! Translates a parsed [`ComposeFile`](crate::compose::types::ComposeFile) into Podman API calls via the libpod REST API.

mod build;
mod container;
mod copy;
pub(crate) mod events;
mod terminal_pump;
pub use events::EventsOptions;
mod image;
pub use build::{BuildOptions, PullOptions, PushOptions};
pub use copy::CpOptions;
pub use image::{resolve_image_digests, CommitOptions};
pub use lifecycle::{validate_stop_timeout, RunOptions, RunOverrides};
pub use lock::ProjectLock;
/// Whether `run` should allocate a pseudo-TTY and attach stdin.
///
/// A TTY on both ends by default, `-T` to opt out, `-d` excluded because there
/// is nobody to be interactive with — and **both** stdin and stdout must be
/// terminals.
///
/// Requiring stdout too is not symmetry for its own sake. A pty merges stdout
/// and stderr and emits CRLF, so `podup run app cmd > out.txt` typed at a shell
/// — stdin still a terminal, stdout a file — would have written pty bytes with
/// stderr folded in, where it used to write clean demultiplexed output. That is
/// the most common redirect there is, and `docker compose` keeps it clean
/// (measured). Checking stdin alone silently changed the format of a file.
pub(crate) fn wants_interactive_run(no_tty: bool, detach: bool) -> bool {
	wants_interactive_with(
		no_tty,
		detach,
		query::stdin_is_terminal(),
		query::stdout_is_terminal(),
	)
}

/// The decision with the environment passed in, so both terminal cases are
/// testable rather than depending on how the suite happened to be invoked.
fn wants_interactive_with(no_tty: bool, detach: bool, stdin_tty: bool, stdout_tty: bool) -> bool {
	!no_tty && !detach && stdin_tty && stdout_tty
}

pub use query::{
	AttachOutcome, ExecOptions, ImagesOptions, LogsDisplay, LogsOptions, PsDisplayOptions,
	PsFilterOptions, PsOptions, DEFAULT_LOG_TAIL,
};
mod container_config;
pub(crate) use container_config::build_log_config;
#[cfg(test)]
mod fake_podman;
mod health;
mod lifecycle;
mod lock;
mod names;
mod network;
mod profiles;
pub use profiles::{retain_active_profiles, retain_active_profiles_with_targets};
mod projects;
pub use projects::{list_projects, list_projects_filtered, LsOptions};
pub(crate) mod query;
mod secrets;
mod staging;
mod stats;
pub use staging::is_safe_project_name;
pub use stats::StatsOptions;
mod volume;
pub use volume::{VolumesDisplayOptions, VolumesOptions};
mod volume_mounts;
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
	/// surface-mode listing is unaffected — `config` is the "show me what
	/// will happen" command, so the warnings stay visible there.
	pub(super) no_warn: bool,
}

impl Engine {
	/// Create an engine for `project_name` using the working directory as the base path for relative volume mounts.
	pub fn new(client: Client, project: String) -> Self {
		Self {
			client,
			project,
			base_dir: std::env::current_dir().unwrap_or_default(),
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
		}
	}

	/// Create an engine with an explicit base directory — use when the compose file is not in the working directory.
	pub fn with_base_dir(client: Client, project: String, base_dir: PathBuf) -> Self {
		Self {
			client,
			project,
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
		service
			.scale
			.or(service.deploy.as_ref().and_then(|d| d.replicas))
			.unwrap_or(1) as usize
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
		// Lock stdout once for the whole stream instead of re-acquiring the lock
		// (and issuing a syscall) per frame; stdout is ours exclusively on this
		// path. stderr is locked per frame because the tracing subscriber also
		// writes there: holding its lock across the await loop would starve
		// concurrent log emissions. Flush after each frame so output stays prompt.
		let mut out = std::io::stdout().lock();
		while let Some(msg) = stream.next().await {
			match msg.map_err(ComposeError::Podman)? {
				LogOutput::StdOut { message } => {
					let _ = out.write_all(String::from_utf8_lossy(&message).as_bytes());
					let _ = out.flush();
				}
				LogOutput::StdErr { message } => {
					let mut err = std::io::stderr().lock();
					let _ = err.write_all(String::from_utf8_lossy(&message).as_bytes());
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
	pub(super) async fn live_replica_name_at(
		&self,
		service_name: &str,
		service: &Service,
		index: Option<u32>,
	) -> Result<String> {
		let names = self.live_replica_names(service_name, service).await?;
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
// Replica resolution helpers
// ---------------------------------------------------------------------------

/// Resolve a replica container name from the set of names that exist for a
/// service (the running replicas, or the statically derived names before
/// anything is created) and a 1-based `--index`. Each name is either the
/// unsuffixed base (the sole replica) or `{base}-{n}`.
///
/// `--index n` targets the replica numbered `n` — by name, not by position —
/// so it stays correct after a runtime `scale`/`up --scale` and regardless of
/// the order Podman lists containers; `0` is rejected (indexes are 1-based);
/// `None` picks the lowest-numbered replica. Pure so it is unit-testable
/// without a Podman socket.
fn resolve_replica_name(
	service_name: &str,
	base: &str,
	names: &[String],
	index: Option<u32>,
) -> Result<String> {
	match index {
		Some(0) => Err(ComposeError::ReplicaIndex {
			service: service_name.to_string(),
			index: 0,
		}),
		Some(i) => {
			let suffixed = format!("{base}-{i}");
			if names.iter().any(|n| n == &suffixed) {
				return Ok(suffixed);
			}
			// A single, unsuffixed replica answers to index 1 only.
			if i == 1 && names.iter().any(|n| n == base) {
				return Ok(base.to_string());
			}
			Err(ComposeError::ReplicaIndex {
				service: service_name.to_string(),
				index: i,
			})
		}
		None => order_replicas(base, names)
			.into_iter()
			.next()
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into())),
	}
}

/// Order replica container names by their 1-based replica number so callers can
/// pick the lowest-numbered one independently of Podman's listing order. A name
/// is the unsuffixed base (the sole replica → number 1) or `{base}-{n}`; names
/// matching neither are dropped.
fn order_replicas(base: &str, names: &[String]) -> Vec<String> {
	let prefix = format!("{base}-");
	let mut numbered: Vec<(usize, String)> = names
		.iter()
		.filter_map(|name| {
			if name == base {
				Some((1, name.clone()))
			} else {
				name.strip_prefix(&prefix)
					.and_then(|s| s.parse::<usize>().ok())
					.map(|n| (n, name.clone()))
			}
		})
		.collect();
	numbered.sort_by_key(|(n, _)| *n);
	numbered.into_iter().map(|(_, name)| name).collect()
}

// ---------------------------------------------------------------------------
// Filesystem helpers
// ---------------------------------------------------------------------------

fn walk_dir(root: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
	let mut out = Vec::new();
	walk_collect(root, &mut out)?;
	Ok(out)
}

fn walk_collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
	let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
	entries.sort_by_key(|e| e.file_name());
	for entry in entries {
		let path = entry.path();
		let file_type = entry.file_type()?;
		out.push(path.clone());
		if file_type.is_dir() {
			walk_collect(&path, out)?;
		}
	}
	Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod to_query_json_tests {
	use super::to_query_json;
	use crate::error::ComposeError;
	use serde::ser::{Error as _, Serializer};
	use serde::Serialize;
	use std::collections::HashMap;

	/// A `Serialize` impl that always errors, so a unit test can pin the
	/// "surface the failure" contract without depending on a particular type
	/// in the build path.
	struct AlwaysFails {
		reason: &'static str,
	}

	impl Serialize for AlwaysFails {
		fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
			Err(S::Error::custom(self.reason))
		}
	}

	/// The happy path: a `Vec<String>` round-trips through `to_query_json`
	/// unchanged. This is the same shape the `cachefrom` and `cacheto` sites
	/// serialise.
	#[test]
	fn to_query_json_serialises_vec_string() {
		let v: Vec<String> = vec!["alpine".into(), "quay.io/lib/alpine".into()];
		assert_eq!(
			to_query_json("build.cache_from", &v).unwrap(),
			r#"["alpine","quay.io/lib/alpine"]"#
		);
	}

	/// The happy path for the `buildargs` and `labels` sites: a `HashMap<String,
	/// String>` serialises to a JSON object.
	#[test]
	fn to_query_json_serialises_hashmap_string_string() {
		let mut m: HashMap<String, String> = HashMap::new();
		m.insert("VERSION".into(), "1.2.3".into());
		let s = to_query_json("build.args", &m).unwrap();
		// Object order isn't stable across HashMap iterations, so parse and
		// check rather than comparing the literal.
		let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
		assert_eq!(parsed["VERSION"], "1.2.3");
	}

	/// The happy path for the `secrets` site: a `Vec<String>` of `id=…,src=…`
	/// specs.
	#[test]
	fn to_query_json_serialises_secrets_vec() {
		let v: Vec<String> = vec!["id=tok,src=.podup-build-secret-tok".into()];
		assert_eq!(
			to_query_json("build.secrets", &v).unwrap(),
			r#"["id=tok,src=.podup-build-secret-tok"]"#
		);
	}

	/// A serialisation failure must surface as `Err(ComposeError::Build)`
	/// whose message names the field and the underlying reason. Each of the
	/// five build sites passes a distinct `what`, so all five labels are
	/// pinned to a test.
	#[test]
	fn to_query_json_failure_names_build_cache_from() {
		let err = to_query_json("build.cache_from", &AlwaysFails { reason: "boom" })
			.expect_err("must surface the error");
		assert!(matches!(err, ComposeError::Build(_)), "got {err:?}");
		let msg = err.to_string();
		assert!(msg.contains("build.cache_from"), "got {msg:?}");
		assert!(msg.contains("boom"), "got {msg:?}");
	}

	#[test]
	fn to_query_json_failure_names_build_args() {
		let err = to_query_json("build.args", &AlwaysFails { reason: "boom" })
			.expect_err("must surface the error");
		assert!(err.to_string().contains("build.args"), "got {err}");
	}

	#[test]
	fn to_query_json_failure_names_build_labels() {
		let err = to_query_json("build.labels", &AlwaysFails { reason: "boom" })
			.expect_err("must surface the error");
		assert!(err.to_string().contains("build.labels"), "got {err}");
	}

	#[test]
	fn to_query_json_failure_names_build_secrets() {
		let err = to_query_json("build.secrets", &AlwaysFails { reason: "boom" })
			.expect_err("must surface the error");
		assert!(err.to_string().contains("build.secrets"), "got {err}");
	}

	#[test]
	fn to_query_json_failure_names_build_cache_to() {
		let err = to_query_json("build.cache_to", &AlwaysFails { reason: "boom" })
			.expect_err("must surface the error");
		assert!(err.to_string().contains("build.cache_to"), "got {err}");
	}
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stream_end_tests;

#[cfg(test)]
mod interactive_run_tests {
	use super::wants_interactive_run;

	/// `-T` opts out, matching `docker compose run` — which has no `-i` because
	/// a TTY on both ends is the default.
	#[test]
	fn no_tty_disables_the_pty() {
		assert!(!wants_interactive_run(true, false));
	}

	/// `-d` detaches, so there is nobody to be interactive with.
	#[test]
	fn detach_disables_the_pty() {
		assert!(!wants_interactive_run(false, true));
	}

	/// The decisive one for existing users: in a test harness — as in any script
	/// or pipeline — stdin is not a terminal, so `run` stays on the unchanged
	/// streaming path. Allocating a pty there would change output framing for
	/// every script that already calls `podup run`.
	#[test]
	fn a_non_terminal_stdin_stays_on_the_streaming_path() {
		assert!(!wants_interactive_run(false, false));
	}

	/// Both ends are required, and this is the case that made it necessary:
	/// `podup run app cmd > out.txt` typed at a shell leaves stdin a terminal
	/// while stdout is a file. Checking stdin alone allocated a pty, and a pty
	/// merges stdout with stderr and writes CRLF — so the redirect silently
	/// changed the bytes the file received. Verified against `docker compose`,
	/// which keeps it clean.
	#[test]
	fn a_redirected_stdout_stays_on_the_streaming_path() {
		assert!(!super::wants_interactive_with(false, false, true, false));
		assert!(super::wants_interactive_with(false, false, true, true));
	}
}
