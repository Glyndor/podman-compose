//! `stats` — live resource-usage stream for a project's service containers.

use std::collections::{HashMap, HashSet};

use futures_util::StreamExt;
use serde::Deserialize;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::types::container::ContainerListEntry;
use crate::libpod::{parse_json_lines, urlencoded, API_PREFIX};

use super::Engine;

/// Options for [`Engine::stats_with_options`], mirroring `docker compose stats`
/// and the table-shaping flags the other list commands expose. Kept off the
/// frozen [`Engine::stats`] signature so the published library API stays stable across minors.
#[derive(Default)]
pub struct StatsOptions {
	/// Disable streaming; print a single snapshot and exit, `--no-stream`.
	pub no_stream: bool,
	/// Include non-running containers as zeroed rows, `-a/--all`.
	pub all: bool,
	/// Emit JSON instead of the table, `--format json`.
	pub json: bool,
	/// Disable container-name truncation in the table, `--no-trunc`.
	pub no_trunc: bool,
}

impl StatsOptions {
	/// Build options from the four CLI flags, in `--no-stream`/`--all`/
	/// `--no-trunc`/`--format json` order. A terse constructor so the CLI keeps
	/// the field names (all `pub`) available for clarity while the dispatch site
	/// stays compact.
	pub fn new(no_stream: bool, all: bool, no_trunc: bool, json: bool) -> Self {
		Self {
			no_stream,
			all,
			no_trunc,
			json,
		}
	}
}

/// Width of the table NAME column; long names are truncated to this width (with
/// a trailing ellipsis) unless `--no-trunc` is given, so a long container name
/// no longer overflows and shifts every following column. Matches [`HEADER`].
const NAME_WIDTH: usize = 32;

/// Build the query fragment scoping a stats request to the `wanted` containers,
/// or an empty string when none are wanted (which falls back to the daemon
/// default). libpod's `/containers/stats` expects the `containers` parameter
/// **repeated** once per container (`&containers=a&containers=b`), not a single
/// comma-joined value — a comma-joined list is parsed as one container name and
/// 404s. Names are sorted for a stable URL and each is URL-encoded.
fn containers_query(wanted: &HashSet<String>) -> String {
	if wanted.is_empty() {
		return String::new();
	}
	let mut names: Vec<&String> = wanted.iter().collect();
	names.sort();
	names
		.iter()
		.map(|n| format!("&containers={}", urlencoded(n)))
		.collect::<String>()
}

/// Whether a `stats --stream` stream that ended with an error broke while a
/// sampled container was still running.
///
/// The stream lives as long as any sampled container runs and ends once none
/// remain, so the error is a real failure only when the re-checked running set
/// still holds one of the containers the stream was sampling. When every
/// sampled container has stopped, the end was expected and the missing terminal
/// frame is the finished-vs-broken ambiguity (#1104), not a fault. Pure so the
/// decision is unit-tested without a live socket.
fn stats_stream_broke_mid_sample(
	sampled: &HashSet<String>,
	still_running: &HashSet<String>,
) -> bool {
	sampled.iter().any(|c| still_running.contains(c))
}

/// Deserialize a map field, treating an explicit JSON `null` as the default
/// (empty) map. libpod sends `"Network": null` for a container with no
/// interfaces, which plain `#[serde(default)]` does not tolerate.
fn null_default<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
	D: serde::Deserializer<'de>,
	T: Default + Deserialize<'de>,
{
	Option::<T>::deserialize(d).map(|v| v.unwrap_or_default())
}

/// One frame of the libpod `/containers/stats` response.
#[derive(Deserialize, Default)]
struct StatsReport {
	#[serde(rename = "Stats", default)]
	stats: Vec<ContainerStat>,
}

/// Per-container resource sample within a [`StatsReport`].
#[derive(Deserialize, Default, Clone)]
struct ContainerStat {
	#[serde(rename = "Name", default)]
	name: String,
	#[serde(rename = "CPU", default)]
	cpu: f64,
	#[serde(rename = "MemUsage", default)]
	mem_usage: u64,
	#[serde(rename = "MemLimit", default)]
	mem_limit: u64,
	#[serde(rename = "MemPerc", default)]
	mem_perc: f64,
	#[serde(rename = "BlockInput", default)]
	block_in: u64,
	#[serde(rename = "BlockOutput", default)]
	block_out: u64,
	#[serde(rename = "PIDs", default)]
	pids: u64,
	// `#[serde(default)]` also tolerates an explicit `null` frame value: libpod
	// sends `"network": null` for a container with no interfaces, which would
	// otherwise fail with `invalid type: null, expected a map`.
	#[serde(rename = "Network", default, deserialize_with = "null_default")]
	network: HashMap<String, NetStat>,
}

/// Per-interface network counters.
#[derive(Deserialize, Default, Clone)]
struct NetStat {
	#[serde(rename = "RxBytes", default)]
	rx: u64,
	#[serde(rename = "TxBytes", default)]
	tx: u64,
}

/// Render a byte count as a compact human string (`1.5MiB`). Pure for testing.
fn format_bytes(bytes: u64) -> String {
	const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
	let mut value = bytes as f64;
	let mut unit = 0;
	while value >= 1024.0 && unit < UNITS.len() - 1 {
		value /= 1024.0;
		unit += 1;
	}
	if unit == 0 {
		format!("{bytes}B")
	} else {
		format!("{value:.1}{}", UNITS[unit])
	}
}

/// Sum a container's per-interface network counters into one `(rx, tx)` pair.
fn net_totals(s: &ContainerStat) -> (u64, u64) {
	s.network
		.values()
		.fold((0u64, 0u64), |(rx, tx), n| (rx + n.rx, tx + n.tx))
}

/// The NAME cell for the table: the full name when `no_trunc`, otherwise
/// truncated to [`NAME_WIDTH`] with a trailing ellipsis so a long name keeps the
/// row aligned. Counts characters (not bytes) so multi-byte names truncate
/// safely. Pure for testing.
fn truncate_name(name: &str, no_trunc: bool) -> String {
	if no_trunc || name.chars().count() <= NAME_WIDTH {
		return name.to_string();
	}
	let head: String = name.chars().take(NAME_WIDTH - 1).collect();
	format!("{head}…")
}

/// Format one stats row into the table layout. With `no_trunc` a long name is
/// left intact (and may overflow its column); otherwise it is truncated to
/// [`NAME_WIDTH`]. Pure for testing.
/// Colour band for a utilisation percentage.
///
/// A container at 95% of its memory limit is minutes from being OOM-killed, and
/// in a plain table that number looks exactly like 0.02%. The whole reason to
/// read `stats` is to find the row that is in trouble, so the number that says
/// so should be the one that catches the eye.
fn load_style(pct: f64) -> crate::ui::Style {
	use crate::ui::AnsiColor;
	let colour = if pct >= 90.0 {
		AnsiColor::Red
	} else if pct >= 70.0 {
		AnsiColor::Yellow
	} else {
		AnsiColor::Green
	};
	crate::ui::Style::new().fg_color(Some(colour.into()))
}

/// Format one stats row into the table layout, optionally coloured. With
/// `no_trunc` a long name is left intact (and may overflow its column);
/// otherwise it is truncated to [`NAME_WIDTH`]. Pure, so the layout is testable
/// without a terminal — the tests pass `colour = false`.
///
/// Each cell is padded to its width *before* being painted: the ANSI codes are
/// zero-width, so padding afterwards would count them and knock every later
/// column out of alignment.
fn format_row_with(s: &ContainerStat, no_trunc: bool, colour: bool) -> String {
	use crate::ui::{identity_style, paint};
	let (rx, tx) = net_totals(s);
	let dim = crate::ui::Style::new().dimmed();

	let name = format!("{:<NAME_WIDTH$}", truncate_name(&s.name, no_trunc));
	let name = paint(identity_style(s.name.trim()), &name, colour);
	let cpu = paint(load_style(s.cpu), &format!("{:>7.2}%", s.cpu), colour);
	let mem_pct = paint(
		load_style(s.mem_perc),
		&format!("{:>6.2}%", s.mem_perc),
		colour,
	);
	// Secondary detail: the absolute figures matter once a percentage has drawn
	// you to the row, not before.
	let mem = paint(
		dim,
		&format!(
			"{:>10} / {:<10}",
			format_bytes(s.mem_usage),
			format_bytes(s.mem_limit)
		),
		colour,
	);
	let net = paint(
		dim,
		&format!("{:>9} / {:<9}", format_bytes(rx), format_bytes(tx)),
		colour,
	);
	let block = paint(
		dim,
		&format!(
			"{:>9} / {:<9}",
			format_bytes(s.block_in),
			format_bytes(s.block_out)
		),
		colour,
	);
	format!("{name} {cpu} {mem} {mem_pct} {net} {block} {:>5}", s.pids)
}

/// Build one `stats --format json` row with numeric values (raw bytes/percent),
/// so machine consumers get exact figures rather than the table's rounded,
/// human-formatted cells. Pure so it can be unit-tested.
fn stat_json_row(s: &ContainerStat) -> serde_json::Value {
	let (rx, tx) = net_totals(s);
	serde_json::json!({
		"Name": s.name,
		"CPUPerc": s.cpu,
		"MemUsage": s.mem_usage,
		"MemLimit": s.mem_limit,
		"MemPerc": s.mem_perc,
		"NetInput": rx,
		"NetOutput": tx,
		"BlockInput": s.block_in,
		"BlockOutput": s.block_out,
		"PIDs": s.pids,
	})
}

const HEADER: &str = "NAME                                 CPU %       MEM USAGE / LIMIT        MEM %    NET I/O             BLOCK I/O           PIDS";

impl Engine {
	/// Stream resource usage for the project's service containers (docker
	/// `compose stats`). Streams continuously until interrupted; `no_stream`
	/// prints a single snapshot. `target_services` narrows to specific services.
	pub async fn stats(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		no_stream: bool,
	) -> Result<()> {
		self.stats_with_options(
			file,
			target_services,
			StatsOptions {
				no_stream,
				..StatsOptions::default()
			},
		)
		.await
	}

	/// Stream resource usage with `docker compose stats`-style options:
	/// `--no-stream` (single snapshot), `-a/--all` (include non-running
	/// containers as zeroed rows), `--format` (table | json), and `--no-trunc`
	/// (keep full container names). `target_services` narrows to specific
	/// services.
	pub async fn stats_with_options(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		opts: StatsOptions,
	) -> Result<()> {
		// Reject unknown/typo service names instead of silently sampling the whole
		// host and printing a header-only table, matching the other commands.
		if let Some(unknown) = first_unknown_service(file, target_services) {
			return Err(ComposeError::ServiceNotFound(unknown.into()));
		}
		let targets = self.target_containers(file, target_services).await?;

		// Only running containers carry live samples, so scope the libpod
		// `containers=` filter to them: a stopped/created container fed to that
		// filter 404s the whole request. Non-running rows are synthesized locally
		// (as zeros) when `--all` is set.
		let running: HashSet<String> = targets
			.iter()
			.filter(|t| t.running)
			.map(|t| t.name.clone())
			.collect();
		let stopped: Vec<String> = if opts.all {
			targets
				.iter()
				.filter(|t| !t.running)
				.map(|t| t.name.clone())
				.collect()
		} else {
			Vec::new()
		};

		// Scope the stats stream to just the running containers server-side via the
		// `containers=` query param, so the daemon does not sample every container
		// on the host (the response is still filtered locally by `running`).
		let containers = containers_query(&running);

		if opts.no_stream || running.is_empty() {
			// Nothing running means nothing to sample (and an empty `containers=`
			// filter would otherwise fall back to the whole host) — skip the call
			// and render an empty/`--all`-only frame.
			let report = if running.is_empty() {
				StatsReport::default()
			} else {
				self.client
					.get_json(&format!(
						"{API_PREFIX}/containers/stats?stream=false{containers}"
					))
					.await
					.map_err(ComposeError::Podman)?
			};
			print_frame(&report, &running, &stopped, &opts);
			return Ok(());
		}

		let resp = self
			.client
			.get_stream(&format!(
				"{API_PREFIX}/containers/stats?stream=true{containers}"
			))
			.await
			.map_err(ComposeError::Podman)?;
		let mut frames = parse_json_lines::<StatsReport>(resp.into_body());
		while let Some(frame) = frames.next().await {
			match frame {
				Ok(report) => print_frame(&report, &running, &stopped, &opts),
				Err(e) => {
					// A `stats --stream` stream lives as long as any sampled
					// container is running, and ends once none remain. libpod
					// signals that end with a chunked terminator, but a lost
					// terminator (a dropped connection, or a version that omits it)
					// reaches here as an `Err` that is *indistinguishable* from a
					// real mid-sample break at the transport layer (#1104). Resolve
					// it out of band: re-check what is still running. If nothing the
					// stream was sampling is alive, the end was expected and the
					// command succeeded; if something is still running, the stream
					// truncated a live sample and the command failed — which is the
					// exit code a monitor scraping `stats` needs (#1080).
					//
					// The re-check is point-in-time: it samples state a moment after
					// the break, so a genuine break that happens to coincide with
					// every sampled container stopping is knowingly tolerated as a
					// clean end — the transport layer cannot tell the two apart, and
					// the sampled containers are gone either way.
					let still_running = match self.target_containers(file, target_services).await {
						Ok(targets) => targets
							.into_iter()
							.filter(|c| c.running)
							.map(|c| c.name)
							.collect::<HashSet<String>>(),
						// Fail closed: an unreadable running set is not confirmation
						// the end was expected, so the original error stands rather
						// than masking a possible failure. Decided here, at the point
						// of the inconclusive re-check, so the guarantee does not
						// depend on the sampled set being non-empty.
						Err(_) => {
							tracing::warn!(
								"stats: stream ended and the running set could not be \
								 re-checked [{}]: {e}",
								e.stream_end_kind()
							);
							return Err(ComposeError::Podman(e));
						}
					};
					if stats_stream_broke_mid_sample(&running, &still_running) {
						tracing::warn!(
							"stats: stream broke while a container was still running [{}]: {e}",
							e.stream_end_kind()
						);
						return Err(ComposeError::Podman(e));
					}
					tracing::debug!(
						"stats: stream ended as its containers stopped [{}]",
						e.stream_end_kind()
					);
					break;
				}
			}
		}
		Ok(())
	}

	/// The containers to report on — every existing replica of the targeted
	/// services (all services when `target_services` is empty), paired with
	/// whether each is currently running. Only containers that actually exist are
	/// returned (no static-name fallback): an absent service simply contributes
	/// no rows.
	async fn target_containers(
		&self,
		file: &ComposeFile,
		target_services: &[String],
	) -> Result<Vec<TargetContainer>> {
		let filters = serde_json::json!({ "label": [format!("podup.project={}", self.project)] });
		let path = format!(
			"{API_PREFIX}/containers/json?all=true&filters={}",
			urlencoded(&filters.to_string()),
		);
		let entries = self
			.client
			.get_json::<Vec<ContainerListEntry>>(&path)
			.await
			.map_err(ComposeError::Podman)?;

		let mut out = Vec::new();
		for e in entries {
			let service = e
				.labels
				.get("podup.service")
				.map(String::as_str)
				.unwrap_or("");
			// Skip containers whose service the compose file no longer defines, and
			// honour a positional `SERVICE` filter.
			if !file.services.contains_key(service) {
				continue;
			}
			if !target_services.is_empty() && !target_services.iter().any(|t| t == service) {
				continue;
			}
			if let Some(raw) = e.names.first() {
				out.push(TargetContainer {
					name: raw.trim_start_matches('/').to_string(),
					running: e.state == "running",
				});
			}
		}
		Ok(out)
	}
}

/// A project container considered for `stats`, with its run state so non-running
/// containers can be folded in (as zeroed rows) only under `--all`.
struct TargetContainer {
	name: String,
	running: bool,
}

/// The first targeted service name that the compose file does not define, if any.
/// Pure so the validation is unit-tested without a live Podman socket.
fn first_unknown_service<'a>(file: &ComposeFile, targets: &'a [String]) -> Option<&'a str> {
	targets
		.iter()
		.map(String::as_str)
		.find(|t| !file.services.contains_key(*t))
}

/// Assemble the rows for one frame: the live samples for `running` containers
/// plus synthesized zero rows for each `stopped` container (already empty when
/// `--all` is off), sorted by name for stable output.
fn frame_rows(
	report: &StatsReport,
	running: &HashSet<String>,
	stopped: &[String],
) -> Vec<ContainerStat> {
	let mut rows: Vec<ContainerStat> = report
		.stats
		.iter()
		.filter(|s| running.contains(&s.name))
		.cloned()
		.collect();
	for name in stopped {
		rows.push(ContainerStat {
			name: name.clone(),
			..ContainerStat::default()
		});
	}
	rows.sort_by(|a, b| a.name.cmp(&b.name));
	rows
}

/// Print one stats frame: the table (a bold header plus one row per container)
/// or a JSON array when `--format json`. Table frames end with a blank line.
fn print_frame(
	report: &StatsReport,
	running: &HashSet<String>,
	stopped: &[String],
	opts: &StatsOptions,
) {
	let rows = frame_rows(report, running, stopped);

	if opts.json {
		let json: Vec<_> = rows.iter().map(stat_json_row).collect();
		// While streaming, one compact array per line — NDJSON, the shape
		// `events` already emits. A pretty-printed array per frame, concatenated,
		// is neither a single JSON document nor NDJSON, so no parser accepts it:
		// `stats --format json` was unreadable by anything for as long as it
		// streamed. `--no-stream` prints one frame and exits, so it stays a
		// single pretty document, which is valid JSON and nicer to read.
		let text = if opts.no_stream {
			serde_json::to_string_pretty(&json)
		} else {
			serde_json::to_string(&json)
		};
		println!("{}", text.unwrap_or_default());
		return;
	}

	crate::ui::print_bold_header(HEADER);
	let colour = crate::ui::stdout_colored();
	for s in &rows {
		println!("{}", format_row_with(s, opts.no_trunc, colour));
	}
	println!();
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
