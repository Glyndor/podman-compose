//! `ps` lists this project's containers as a table or JSON. Split out of the
//! query root so each file stays within the source line limit.

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::{ContainerListEntry, ContainerPort};
use crate::libpod::{urlencoded, API_PREFIX};

use super::Engine;
use crate::units::{format_bytes, format_duration, DurationFormat, SizeFormat};

use super::inspect_util::humanize_age;

/// Options for [`Engine::ps_with_options`], mirroring `docker compose ps`.
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`PsOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Default)]
#[non_exhaustive]
pub struct PsOptions {
	/// Include stopped containers, `-a/--all` (default: running only).
	pub all: bool,
	/// Print only container IDs, `-q/--quiet`.
	pub quiet: bool,
	/// Emit JSON instead of the table, `--format json`.
	pub json: bool,
}

impl PsOptions {
	/// Every `docker compose ps` flag, in CLI order. A constructor rather than
	/// a struct literal because the type is `#[non_exhaustive]`, so the next
	/// flag to land is not a breaking change for anyone building one.
	pub fn new(all: bool, quiet: bool, json: bool) -> Self {
		Self { all, quiet, json }
	}

	/// Include stopped containers, `-a/--all` (default: running only).
	/// Builder-style.
	#[must_use]
	pub fn with_all(mut self, all: bool) -> Self {
		self.all = all;
		self
	}

	/// Print only container IDs, `-q/--quiet`. Builder-style.
	#[must_use]
	pub fn with_quiet(mut self, quiet: bool) -> Self {
		self.quiet = quiet;
		self
	}

	/// Emit JSON instead of the table, `--format json`. Builder-style.
	#[must_use]
	pub fn with_json(mut self, json: bool) -> Self {
		self.json = json;
		self
	}
}

/// Options for `ps` added after the crate's API froze.
///
/// **`#[non_exhaustive]` from birth, and that is the whole point.** Both
/// [`PsOptions`] and [`PsFilterOptions`] are externally constructible with a
/// struct literal, so adding a field to either requires a MAJOR, measured with
/// `cargo semver-checks`, which reports `constructible_struct_adds_field`, not
/// assumed from the rules. `PsFilterOptions` was itself introduced to keep
/// `PsOptions` stable and inherited the same problem, so a third frozen struct
/// would only move the wall.
///
/// Construct it with [`Default::default`] and the builder below; a struct
/// literal is refused outside this crate, which is what buys the room to grow.
#[derive(Default, Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PsDisplayOptions {
	/// Ask the server for each container's on-disk size and show the SIZE
	/// column, `-s/--size`.
	///
	/// Off by default because it is not free: libpod walks each container's
	/// writable layer to answer, measured at 21 ms → 109 ms over 59 containers
	/// on Podman 5.7.0. `docker ps -s` is opt-in for the same reason.
	pub size: bool,
}

impl PsDisplayOptions {
	/// Ask for the SIZE column, `-s/--size`. Builder-style.
	#[must_use]
	pub fn with_size(mut self, size: bool) -> Self {
		self.size = size;
		self
	}

	/// The single `ps` display flag, in CLI order. A constructor rather than
	/// a struct literal because the type is `#[non_exhaustive]`, so the next
	/// flag to land is not a breaking change for anyone building one.
	pub fn new(size: bool) -> Self {
		Self { size }
	}
}

/// Service/status/name filters for [`Engine::ps_filtered`] (`docker compose ps`
/// `--services`, `[SERVICE...]`, `--status`, `--filter`).
///
/// `#[non_exhaustive]` since 4.0.0, same rationale as [`PsOptions`]: a new
/// filter kind can be added in a minor release without breaking external
/// callers. Construct it via [`PsFilterOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate.
#[derive(Default)]
#[non_exhaustive]
pub struct PsFilterOptions {
	/// Print the service names instead of the container table, `--services`.
	pub services_only: bool,
	/// Restrict to these services' containers (positional `SERVICE` filter).
	pub services: Vec<String>,
	/// Status filters, `--status` (e.g. running, exited); OR-combined.
	pub status: Vec<String>,
	/// Generic `KEY=VALUE` predicates, `--filter` (supports status= and name=).
	pub filters: Vec<String>,
}

impl PsFilterOptions {
	/// Every `ps` filter flag, in CLI order. A constructor rather than a struct
	/// literal because the type is `#[non_exhaustive]`, so the next flag to
	/// land is not a breaking change for anyone building one.
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		services_only: bool,
		services: Vec<String>,
		status: Vec<String>,
		filters: Vec<String>,
	) -> Self {
		Self {
			services_only,
			services,
			status,
			filters,
		}
	}

	/// Print the service names instead of the container table, `--services`.
	/// Builder-style.
	#[must_use]
	pub fn with_services_only(mut self, services_only: bool) -> Self {
		self.services_only = services_only;
		self
	}

	/// Restrict to these services' containers (positional `SERVICE` filter).
	/// Builder-style.
	#[must_use]
	pub fn with_services(mut self, services: Vec<String>) -> Self {
		self.services = services;
		self
	}

	/// Status filters, `--status` (e.g. running, exited); OR-combined.
	/// Builder-style.
	#[must_use]
	pub fn with_status(mut self, status: Vec<String>) -> Self {
		self.status = status;
		self
	}

	/// Generic `KEY=VALUE` predicates, `--filter` (supports status= and name=).
	/// Builder-style.
	#[must_use]
	pub fn with_filters(mut self, filters: Vec<String>) -> Self {
		self.filters = filters;
		self
	}
}

/// Human-readable status for `ps`. Podman's libpod list endpoint leaves
/// `Status` empty and reports the machine state in `State`, so fall back to it
/// rather than rendering a blank column.
fn display_status(c: &ContainerListEntry) -> &str {
	if c.status.is_empty() {
		&c.state
	} else {
		&c.status
	}
}

/// How `ps` renders a span: the shared default, three components.
const SPAN_FORMAT: DurationFormat = DurationFormat::default_parts();

/// Table STATUS cell, as of `now` (Unix seconds).
///
/// A running container says how long it has been up, which is what both
/// reference tools do and what podup did not: `podman ps` renders `Up 13 hours
/// (healthy)` and `docker compose ps` renders `Up 2 minutes`, while podup
/// rendered the bare word `running`. The state alone answers "is it on"; the
/// span answers "did it just restart", which is the question someone runs `ps`
/// to settle.
///
/// The health suffix only appears when the container has a healthcheck:
/// libpod leaves `Status` empty otherwise, measured on Podman 5.7.0, so there is
/// nothing to append rather than an unknown to invent.
///
/// Podman's list endpoint reports an exited container with a bare `exited` state
/// and no code, which is indistinguishable from a clean exit; surface the exit
/// code the way `docker compose ps` does (`Exited (0)` / `Exited (7)`).
///
/// `now` is a parameter rather than a clock read so the rendering is pure and
/// its tests are deterministic.
fn table_status(c: &ContainerListEntry, now: i64) -> String {
	let status = display_status(c);
	let exited = c.state.eq_ignore_ascii_case("exited") || c.state.eq_ignore_ascii_case("dead");
	// Only synthesize when the status text doesn't already carry the code, so a
	// richer Docker-style `Exited (7) 4 seconds ago` is left untouched.
	if exited && !status.contains("Exited (") {
		return format!("Exited ({})", c.exit_code.unwrap_or(0));
	}
	if !c.state.eq_ignore_ascii_case("running") {
		return status.to_string();
	}
	let Some(uptime) = span_since(c.started_at, now) else {
		return status.to_string();
	};
	// `health_from_status` answers with an empty string when the container has
	// no healthcheck, which is the same case as libpod sending an empty
	// `Status`, so nothing to append rather than an unknown to invent.
	match health_from_status(status) {
		"" => format!("Up {uptime}"),
		health => format!("Up {uptime} ({health})"),
	}
}

/// The libpod list request `ps` makes, as a URL.
///
/// Pure so the query it builds can be asserted without a server. The `size`
/// parameter is the one worth pinning: `size=true` is not a bigger payload, it
/// is work: libpod walks each container's writable layer to answer, measured
/// at 21 ms against 109 ms over 59 containers on Podman 5.7.0, so asking for it
/// unconditionally would make every `ps` pay for a column most readers do not
/// look at. `docker ps -s` is opt-in for the same reason.
///
/// Extracted after a mutation that hard-coded `size=true` survived every test:
/// the string was built inside an async method that needs a live socket, so
/// nothing could reach it.
fn containers_path(project: &str, all: bool, size: bool) -> String {
	let label = format!("podup.project={project}");
	let filters = serde_json::json!({ "label": [label] });
	format!(
		"{API_PREFIX}/containers/json?all={all}&size={size}&filters={}",
		urlencoded(&filters.to_string()),
	)
}

/// How `ps` renders a size: decimal units at three significant digits.
///
/// The same shape `images` uses, and for the same reason: this column is read
/// against `podman ps -s`, which prints `143kB (virtual 225MB)`. Measured
/// against it rather than assumed.
const SIZE_FORMAT: SizeFormat = SizeFormat::decimal().with_significant(3);

/// Table SIZE cell: the writable layer, then the image behind it.
///
/// `143kB (virtual 225MB)`, matching `podman ps -s` and `docker ps -s`.
/// **`virtual` is the image's own size, not the sum**, verified on three
/// containers whose two readings differ at three significant digits, because on
/// a container with a small writable layer the two are indistinguishable and a
/// wrong choice would never show.
///
/// Empty when the server sent no size, which is what it does unless the request
/// asked. A blank cell says podup did not ask; `0B` would claim it did.
fn table_size(c: &ContainerListEntry) -> String {
	let Some(size) = &c.size else {
		return String::new();
	};
	format!(
		"{} (virtual {})",
		format_bytes(size.rw, &SIZE_FORMAT),
		format_bytes(size.root_fs, &SIZE_FORMAT)
	)
}

/// Table CREATED cell, as of `now`: how long ago the container was created.
///
/// Empty when libpod sent no timestamp or one this cannot parse. A blank cell
/// says podup could not tell; a cell holding a plausible wrong age does not, and
/// the wrong age is the one a reader acts on.
fn table_created(c: &ContainerListEntry, now: i64) -> String {
	let Some(secs) = crate::timestamp::parse_rfc3339(&c.created) else {
		return String::new();
	};
	if secs <= 0 {
		return String::new();
	}
	humanize_age(now.saturating_sub(secs).max(0))
}

/// Render the span from `then` to `now`, or `None` when there is nothing to
/// render.
///
/// A zero `then` means the field was absent, not that the instant was the epoch.
/// A `then` in the future is clock skew between this process and the server,
/// clamped to zero rather than rendered as a negative age, since `Up 0s` on a
/// container that just started is right and `Up -3s` is never right.
fn span_since(then: i64, now: i64) -> Option<String> {
	if then <= 0 {
		return None;
	}
	let elapsed = now.saturating_sub(then).max(0);
	Some(format_duration(
		std::time::Duration::from_secs(elapsed as u64),
		&SPAN_FORMAT,
	))
}

/// The wall clock as Unix seconds, for the cells that render an age.
///
/// Before the epoch is not a state this can reach on any host that boots, so a
/// failure floors at zero and every cell renders blank rather than the call
/// site handling an error it cannot act on.
pub(super) fn now_unix() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_secs() as i64)
		.unwrap_or(0)
}

/// The container's display name (leading slash stripped).
fn name_of(c: &ContainerListEntry) -> String {
	c.names.join(", ").trim_start_matches('/').to_string()
}

/// Whether a container status/state word satisfies a `--status`/`status=` filter.
/// Each wanted value matches case-insensitively as a prefix of the status word
/// (so `running` matches `running` and `up`-style strings via the state). An
/// empty `wanted` matches everything. Pure so the predicate is unit-tested.
fn status_matches(status: &str, wanted: &[String]) -> bool {
	if wanted.is_empty() {
		return true;
	}
	let s = status.trim().to_ascii_lowercase();
	wanted.iter().any(|w| {
		let w = w.trim().to_ascii_lowercase();
		!w.is_empty() && (s == w || s.starts_with(&w))
	})
}

/// Split `--filter KEY=VALUE` predicates into the supported buckets: extra
/// `status=` values are folded into the status filter, `name=` values into the
/// name-substring filter, and anything else is returned as `unknown` so the
/// caller can warn. Pure so it is unit-tested.
fn split_ps_filters(filters: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
	let (mut status, mut names, mut unknown) = (Vec::new(), Vec::new(), Vec::new());
	for f in filters {
		match f.split_once('=') {
			Some(("status", v)) => status.push(v.to_string()),
			Some(("name", v)) => names.push(v.to_string()),
			_ => unknown.push(f.clone()),
		}
	}
	(status, names, unknown)
}

/// The health word embedded in a human status string (`Up 2 minutes (healthy)`),
/// or `""` when absent. The libpod list endpoint carries no separate health
/// field, so `ps` derives it from the status text the way `docker ps` shows it.
fn health_from_status(status: &str) -> &'static str {
	let s = status.to_ascii_lowercase();
	if s.contains("unhealthy") {
		"unhealthy"
	} else if s.contains("healthy") {
		"healthy"
	} else if s.contains("health: starting") {
		"starting"
	} else {
		""
	}
}

/// Number of consecutive ports a record covers (`range`, at least 1).
fn span_len(p: &ContainerPort) -> u16 {
	p.range.filter(|&r| r > 0).unwrap_or(1)
}

/// `base + offset` widened to `u32` before adding. `host_port`,
/// `container_port` and `range` all come straight off libpod's JSON, untrusted
/// input a hostile or buggy daemon could set to any `u16` value, so a plain
/// `u16 + u16` (e.g. `host_port: 65535` with a `range` of 2) can overflow: it
/// wraps silently in a release build and panics under overflow-checks. Doing
/// the addition in `u32` keeps every legitimate port value identical while a
/// pathological one renders as the (larger) real number instead of wrapping.
fn widen_add(base: u16, offset: u16) -> u32 {
	u32::from(base) + u32::from(offset)
}

/// Host IP for display: an unset/empty value means all interfaces (`0.0.0.0`),
/// matching Docker/Podman output (libpod commonly omits it).
fn display_host_ip(p: &ContainerPort) -> &str {
	p.host_ip
		.as_deref()
		.filter(|s| !s.is_empty())
		.unwrap_or("0.0.0.0")
}

/// Render one port record the way `docker compose ps` does. A collapsed range
/// (`range > 1`) is rendered as `host_start-host_end->cont_start-cont_end` so the
/// whole range is shown rather than only its first mapping.
fn format_port_record(p: &ContainerPort) -> String {
	let proto = p
		.protocol
		.as_deref()
		.map(|proto| format!("/{proto}"))
		.unwrap_or_default();
	let host_ip = display_host_ip(p);
	let hp = p.host_port.unwrap_or(0);
	let n = span_len(p);
	if n > 1 {
		format!(
			"{host_ip}:{hp}-{}->{}-{}{proto}",
			widen_add(hp, n - 1),
			p.container_port,
			widen_add(p.container_port, n - 1),
		)
	} else {
		format!("{host_ip}:{hp}->{}{proto}", p.container_port)
	}
}

/// Render a container's published ports as a comma-joined `ps` PORTS cell.
fn format_ports(ports: &[ContainerPort]) -> String {
	ports
		.iter()
		.map(format_port_record)
		.collect::<Vec<_>>()
		.join(", ")
}

/// Structured publishers for `ps --format json`, one object per published port
/// (a collapsed range is expanded so every port appears), mirroring the
/// `Publishers` array docker compose emits.
fn publishers(ports: &[ContainerPort]) -> Vec<serde_json::Value> {
	let mut out = Vec::new();
	for p in ports {
		let n = span_len(p);
		for i in 0..n {
			out.push(serde_json::json!({
				"URL": display_host_ip(p),
				"TargetPort": widen_add(p.container_port, i),
				"PublishedPort": p.host_port.map(|hp| widen_add(hp, i)),
				"Protocol": p.protocol.as_deref().unwrap_or("tcp"),
			}));
		}
	}
	out
}

/// Build one `ps --format json` row, surfacing the fields docker compose
/// machine consumers expect (Service/State/Health/ExitCode/Publishers) in
/// addition to Name/Image/Status/ID. Pure so it can be unit-tested.
fn ps_json_row(c: &ContainerListEntry) -> serde_json::Value {
	serde_json::json!({
		"Name": name_of(c),
		"Image": c.image,
		"Project": c.labels.get("podup.project").cloned().unwrap_or_default(),
		"Service": c.labels.get("podup.service").cloned().unwrap_or_default(),
		"State": c.state,
		"Status": display_status(c),
		"Health": health_from_status(display_status(c)),
		"ExitCode": c.exit_code.unwrap_or(0),
		"Publishers": publishers(&c.ports),
		// The raw wire values, not a rendering. `docker compose ps --format
		// json` passes the RFC 3339 string through too, and a machine consumer
		// wants an instant it can compute with rather than `2mo 1d`.
		"Created": c.created,
		"StartedAt": c.started_at,
		// Raw byte counts, and null when the size was not requested, the same
		// distinction the table draws between an empty cell and a zero.
		"Size": c.size.map(|s| serde_json::json!({
			"RwSize": s.rw,
			"RootFsSize": s.root_fs,
		})),
		"ID": c.id,
	})
}

impl Engine {
	/// List running containers for this project as a table (default options).
	pub async fn ps(&self, file: &ComposeFile) -> Result<()> {
		self.ps_with_options(file, PsOptions::default()).await
	}

	/// List containers with `docker compose ps`-style options (`-a/--all`,
	/// `-q/--quiet`, `--format`). For the `--services`/`[SERVICE...]`/`--status`/
	/// `--filter` predicates use [`Engine::ps_filtered`].
	pub async fn ps_with_options(&self, file: &ComposeFile, opts: PsOptions) -> Result<()> {
		self.ps_filtered(file, opts, PsFilterOptions::default())
			.await
	}

	/// List containers with `docker compose ps`-style options: `-a/--all`
	/// (include stopped), `-q/--quiet` (full IDs only), `--format`
	/// (table | json), `--services` (service-name list), a positional `SERVICE`
	/// filter, and `--status`/`--filter` predicates.
	pub async fn ps_filtered(
		&self,
		file: &ComposeFile,
		opts: PsOptions,
		filters: PsFilterOptions,
	) -> Result<()> {
		self.ps_filtered_with_display(file, opts, filters, PsDisplayOptions::default())
			.await
	}

	/// Like [`Engine::ps_filtered`], plus the options added after the API froze
	/// ([`PsDisplayOptions`]). A separate entry point rather than a fourth
	/// parameter on the old one, so existing callers keep compiling.
	pub async fn ps_filtered_with_display(
		&self,
		file: &ComposeFile,
		opts: PsOptions,
		filters: PsFilterOptions,
		display: PsDisplayOptions,
	) -> Result<()> {
		for name in &filters.services {
			if !file.services.contains_key(name) {
				return Err(ComposeError::ServiceNotFound(name.clone()));
			}
		}

		// `--services` lists the (optionally filtered) configured service names,
		// one per line, instead of the container table.
		if filters.services_only {
			for name in file.services.keys() {
				if filters.services.is_empty() || filters.services.iter().any(|s| s == name) {
					println!("{name}");
				}
			}
			return Ok(());
		}

		// Fold `--status` and any `status=`/`name=` from `--filter` together. An
		// unsupported key is an error, not a warning: a dropped predicate means
		// the command answers a question the caller did not ask, and a script
		// filtering for a condition reads the unfiltered set back as a match.
		// docker compose errors here too.
		let (mut status_filter, name_filter, unknown) = split_ps_filters(&filters.filters);
		if let Some(u) = unknown.first() {
			return Err(ComposeError::Unsupported(format!(
				"unsupported ps filter {u:?}: expected name=<NAME> or status=<STATE>"
			)));
		}
		status_filter.extend(filters.status.iter().cloned());

		// A positional `SERVICE` filter restricts to those services' container
		// names (across replicas).
		let allowed_names: Option<std::collections::HashSet<String>> =
			if filters.services.is_empty() {
				None
			} else {
				Some(
					filters
						.services
						.iter()
						.filter_map(|n| file.services.get(n).map(|s| (n, s)))
						.flat_map(|(n, s)| self.replica_names(n, s))
						.collect(),
				)
			};

		// A status filter (`--status exited`, `--filter status=exited`) implies
		// querying every container regardless of state: libpod's list endpoint
		// with `all=false` returns only running containers, so `ps --status
		// exited` without `-a` would always come back empty. `status_matches`
		// below still narrows the result to the requested status(es); this only
		// widens what libpod itself is asked for.
		let all = opts.all || !status_filter.is_empty();
		let path = containers_path(&self.project, all, display.size);

		let all_containers = self
			.client
			.get_json::<Vec<ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;

		let containers: Vec<ContainerListEntry> = all_containers
			.into_iter()
			.filter(|c| {
				// `x-podman-pod`: hide the infra container Podman creates
				// inside the project pod. The pod addresses itself by name
				// (`podman pod`), so the infra container has no user-facing
				// role.
				if c.is_infra {
					return false;
				}
				let name = name_of(c);
				allowed_names.as_ref().is_none_or(|set| {
					c.names
						.iter()
						.any(|n| set.contains(n.trim_start_matches('/')))
				}) && (status_matches(&c.state, &status_filter)
					|| status_matches(&c.status, &status_filter))
					&& (name_filter.is_empty() || name_filter.iter().any(|nf| name.contains(nf)))
			})
			.collect();

		if opts.quiet {
			// Full 64-char IDs, like `docker compose ps -q` (and podup's JSON),
			// so scripts consuming the IDs are not handed truncated values.
			for c in &containers {
				println!("{}", c.id);
			}
			return Ok(());
		}

		if opts.json {
			let rows: Vec<_> = containers.iter().map(ps_json_row).collect();
			println!("{}", super::super::to_pretty_json("ps.row", &rows)?);
			return Ok(());
		}

		// One clock read for the whole table, so two rows created in the same
		// second cannot render different ages.
		let now = now_unix();
		// SIZE is appended rather than inserted, so a reader's existing column
		// positions do not move when the flag is off, and `docker ps -s` puts
		// it last too.
		let mut headers: Vec<&str> = vec!["NAME", "IMAGE", "CREATED", "STATUS", "PORTS"];
		if display.size {
			headers.push("SIZE");
		}
		let mut table = crate::ui::Table::new(&headers)
			.cap(0, 48)
			.cap(1, 48)
			.status_col(3)
			.identity_col(0);
		for c in &containers {
			let mut row = vec![
				name_of(c),
				c.image.clone(),
				table_created(c, now),
				table_status(c, now),
				format_ports(&c.ports),
			];
			if display.size {
				row.push(table_size(c));
			}
			table.push(row);
		}
		if table.is_empty() {
			// An empty ps table is a legitimate answer; print the explicit
			// "no containers" line on stderr so a script capturing stdout (or
			// `--format json`) sees nothing (#1675).
			crate::ui::progress_note("no containers");
			return Ok(());
		}
		table.print();

		Ok(())
	}
}

/// Test-only: returns the JSON row map a `ps` invocation would render, so
/// the unit tests can introspect the filtered container set without going
/// through stdout.
#[cfg(test)]
pub(crate) async fn ps_rows_for_test(
	engine: &Engine,
	file: &ComposeFile,
) -> Result<Vec<serde_json::Value>> {
	let opts = PsOptions::new(true, false, true);
	let filters = PsFilterOptions::default();
	let display = PsDisplayOptions::default();
	// Re-run the same filter the production path runs, but capture the
	// resulting rows instead of printing them.
	let path = containers_path(&engine.project, opts.all || true, display.size);
	let all_containers = engine
		.client
		.get_json::<Vec<ContainerListEntry>>(&path)
		.await
		.map_err(ComposeError::Podman)?;
	let allowed_names: Option<std::collections::HashSet<String>> = if filters.services.is_empty() {
		None
	} else {
		Some(
			filters
				.services
				.iter()
				.filter_map(|n| file.services.get(n).map(|s| (n, s)))
				.flat_map(|(n, s)| engine.replica_names(n, s))
				.collect(),
		)
	};
	let (status_filter, name_filter, _) = split_ps_filters(&filters.filters);
	let containers: Vec<ContainerListEntry> = all_containers
		.into_iter()
		.filter(|c| {
			if c.is_infra {
				return false;
			}
			let name = name_of(c);
			allowed_names.as_ref().is_none_or(|set| {
				c.names
					.iter()
					.any(|n| set.contains(n.trim_start_matches('/')))
			}) && (status_matches(&c.state, &status_filter)
				|| status_matches(&c.status, &status_filter))
				&& (name_filter.is_empty() || name_filter.iter().any(|nf| name.contains(nf)))
		})
		.collect();
	Ok(containers.iter().map(ps_json_row).collect())
}

#[cfg(test)]
mod tests;
