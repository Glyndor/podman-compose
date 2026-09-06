//! `events` streams Podman events scoped to the project (`docker compose
//! events`). Filters the libpod event stream by the `podup.project` label.

use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::Engine;

/// Options for [`Engine::stream_events`], mirroring `docker compose events`
/// (`--since`, `--until`, `--filter`).
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`EventsOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct EventsOptions {
	/// Only events at or after this timestamp/relative time (`--since`).
	pub since: Option<String>,
	/// Only events up to this timestamp/relative time (`--until`).
	pub until: Option<String>,
	/// Extra `KEY=VALUE` event filters (`--filter`, e.g. `event=start`).
	pub filters: Vec<String>,
}

impl EventsOptions {
	/// Every `docker compose events` flag, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next flag to land is not a breaking change for anyone building one.
	pub fn new(since: Option<String>, until: Option<String>, filters: Vec<String>) -> Self {
		Self {
			since,
			until,
			filters,
		}
	}

	/// Only events at or after this timestamp/relative time (`--since`).
	/// Builder-style.
	#[must_use]
	pub fn with_since(mut self, since: Option<String>) -> Self {
		self.since = since;
		self
	}

	/// Only events up to this timestamp/relative time (`--until`).
	/// Builder-style.
	#[must_use]
	pub fn with_until(mut self, until: Option<String>) -> Self {
		self.until = until;
		self
	}

	/// Extra `KEY=VALUE` event filters (`--filter`, e.g. `event=start`).
	/// Builder-style.
	#[must_use]
	pub fn with_filters(mut self, filters: Vec<String>) -> Self {
		self.filters = filters;
		self
	}
}

impl Engine {
	/// Stream events for this project's containers. With `json`, each event is
	/// printed as a compact JSON line; otherwise as `TYPE ACTION NAME`.
	///
	/// The feed is unbounded, so it normally ends only when the caller stops it.
	/// Returning at all therefore means the stream was lost, and this returns
	/// `Err`; see [`Engine::stream_events_with_options`] for the bounded case.
	pub async fn stream_events(&self, json: bool) -> Result<()> {
		self.stream_events_with_options(json, &EventsOptions::default())
			.await
	}

	/// [`Engine::stream_events`] with `docker compose events`-style `--since`,
	/// `--until`, and `--filter` options.
	///
	/// # Errors
	///
	/// A transport failure always returns the underlying error, whatever was
	/// asked for. Beyond that, whether a *clean* ending is an error depends on
	/// what the caller asked for:
	///
	/// - **`since` and `until` both set, both already elapsed**: the window
	///   closes on its own, so a clean ending is what was asked for. Returns
	///   `Ok(())`.
	/// - **anything else**: the feed is unbounded and libpod never ends it, so
	///   any ending means the stream was lost. Returns
	///   [`ComposeError::StreamTruncated`](crate::ComposeError::StreamTruncated).
	///
	/// Also returns `Err` if a `--filter` is malformed or the stream cannot be
	/// opened.
	///
	/// A window needs **both** ends to close, and both must already have
	/// elapsed. Measured against Podman 5.4.2: `since` and `until` together
	/// close the feed, whether absolute or relative (`-2h`..`-1h`); either one
	/// alone leaves it open, as does any `until` in the future. So `--until 5m`
	/// follows indefinitely rather than stopping in five minutes, and `--until
	/// -5m` alone does too.
	pub async fn stream_events_with_options(&self, json: bool, opts: &EventsOptions) -> Result<()> {
		let filters = build_event_filters(&self.project, &opts.filters)?;
		let mut path = format!(
			"{API_PREFIX}/events?stream=true&filters={}",
			urlencoded(&filters.to_string()),
		);
		if let Some(since) = &opts.since {
			path.push_str(&format!("&since={}", urlencoded(since)));
		}
		if let Some(until) = &opts.until {
			path.push_str(&format!("&until={}", urlencoded(until)));
		}
		let resp = self
			.client
			.get_stream(&path)
			.await
			.map_err(ComposeError::Podman)?;
		let mut stream = crate::libpod::parse_json_lines::<Value>(resp.into_body());
		// Whether a clean end was expected is decided by what was asked for, not
		// by the shape of the end.
		//
		// The other streaming commands re-check the container they followed
		// (#1169, #1204, #1242). An events feed is project-scoped and follows no
		// single container, so there is nothing to re-check. What the client
		// asked for answers it instead, at no API cost.
		//
		// Keying on the request rather than on `Err` versus a clean `None`
		// matters: a closed window ends the feed *cleanly*, so an unbounded feed
		// reaching `None` is the same anomaly as one reaching `Err`, and the
		// previous code exited 0 for both.
		//
		// A window needs BOTH ends to close. Measured against 5.4.2 with curl,
		// no podup involved, `stream=true`:
		//
		//   since + until, both past, absolute   closes
		//   since + until, relative (-2h..-1h)   closes
		//   until alone, past                    stays open
		//   since alone, past                    stays open
		//
		// So `--until` on its own does not bound anything, whatever the user
		// meant by it, and treating it as intent-to-bound would call an unbounded
		// feed bounded. A future `until` never closes either, with or without
		// `since`.
		let bounded = opts.since.is_some() && opts.until.is_some();
		if opts.until.is_some() && opts.since.is_none() {
			tracing::warn!(
				"events: --until without --since does not bound the feed; libpod keeps it open. \
				 Pass both to bound a window."
			);
		}
		// No header on the machine path: `--format json` is what a parser reads.
		if !json {
			crate::ui::print_bold_header(&events_header());
		}
		let mut broke: Option<crate::libpod::PodmanError> = None;
		while let Some(event) = stream.next().await {
			match event {
				Ok(value) => println!("{}", format_event(&value, json)),
				Err(e) => {
					tracing::warn!("events: stream ended early [{}]: {e}", e.stream_end_kind());
					broke = Some(e);
					break;
				}
			}
		}
		// A transport failure is never expected, whatever was asked for. Intent
		// says whether an *ending* was expected; it cannot make a severed socket
		// expected. Reading it as "bounded means always fine" inverted the whole
		// point of #1104 on the scriptable path: `--until` with `--format json`
		// is what a script uses, and it would have truncated its window and
		// reported success, while the interactive unbounded form got the strict
		// check.
		if let Some(e) = broke {
			return Err(ComposeError::Podman(e));
		}
		if bounded {
			return Ok(());
		}
		// An unbounded feed that ended cleanly still lost its connection: libpod
		// does not end one, so reaching here at all is the anomaly.
		Err(ComposeError::StreamTruncated(
			"events stream ended on its own: an unbounded feed only ends when the client stops it"
				.to_string(),
		))
	}
}

/// Build the libpod events `filters` object: always scope to this project's
/// `podup.project` label, then merge each user `KEY=VALUE` predicate (appending
/// to that key's value array). Pure so the merge is unit-tested.
///
/// A predicate with no `=` is an error rather than a skip. Dropping it scoped
/// the stream to the whole project instead: `events --filter garbage` printed
/// everything, which a caller reads as "these all matched". docker compose
/// errors on a malformed filter too.
fn build_event_filters(project: &str, user_filters: &[String]) -> Result<Value> {
	use serde_json::{Map, Value};
	let mut map: Map<String, Value> = Map::new();
	map.insert(
		"label".to_string(),
		Value::Array(vec![Value::String(format!("podup.project={project}"))]),
	);
	for f in user_filters {
		let Some((key, value)) = f.split_once('=') else {
			return Err(ComposeError::Unsupported(format!(
				"malformed events filter {f:?}: expected KEY=VALUE (e.g. event=start)"
			)));
		};
		match map
			.entry(key.to_string())
			.or_insert_with(|| Value::Array(Vec::new()))
		{
			Value::Array(arr) => arr.push(Value::String(value.to_string())),
			other => *other = Value::Array(vec![Value::String(value.to_string())]),
		}
	}
	Ok(Value::Object(map))
}

/// Render one event. `json` emits the raw object as a compact line; otherwise a
/// `TYPE ACTION NAME` summary, tolerant of both the docker-compat shape
/// (`Type`/`Action`/`Actor.Attributes.name`) and the libpod-native one
/// (`status`/`id`).
fn format_event(value: &Value, json: bool) -> String {
	if json {
		// The `--format json` consumer is parsing this line by line, so a
		// serialisation failure cannot propagate up as an `Err` without
		// truncating the NDJSON stream. Surface the cause at `debug` (the
		// operator who runs with `RUST_LOG=debug` sees why one row is
		// missing), drop the row, and let the stream continue (#1366).
		return match super::to_query_json("events row", value) {
			Ok(s) => s,
			Err(e) => {
				tracing::debug!("events: dropping unserialisable row: {e}");
				String::new()
			}
		};
	}
	let typ = value.get("Type").and_then(Value::as_str).unwrap_or("");
	let action = value
		.get("Action")
		.or_else(|| value.get("status"))
		.and_then(Value::as_str)
		.unwrap_or("");
	let name = value
		.pointer("/Actor/Attributes/name")
		.or_else(|| value.get("id"))
		.and_then(Value::as_str)
		.unwrap_or("");
	// libpod sends `time` (seconds) alongside `timeNano`; seconds is the one the
	// column needs. An event without it renders the field blank rather than
	// inventing "now", which would be a timestamp that looks real and is not.
	let time = value.get("time").and_then(Value::as_i64);
	format_event_line(
		time,
		typ,
		action,
		name,
		crate::ui::stdout_colored() && !json,
	)
}
/// Render an event's timestamp in the reader's own time zone, with the offset
/// that applied at that instant.
///
/// The calendar arithmetic used to live here as its own civil-from-days walk.
/// It moved to `crate::timestamp`, which already held the inverse for parsing:
/// the two halves are one thing, and a round-trip test between them only means
/// something while neither can be edited without the other in view.
pub(crate) fn format_event_time(unix_secs: i64) -> String {
	crate::timestamp::format_local(unix_secs)
}

/// Display width of the `TIME` column.
///
/// `YYYY-MM-DD HH:MM:SS -05:00` is twenty-six. It was nineteen while the column
/// rendered UTC and said nothing about it; adding the offset without widening
/// truncated every row to `...19:00:0…`, which the tests caught. The two belong
/// in one edit: a width that describes a format it no longer holds is the same
/// class of bug as the `stats` header that had drifted off its own columns.
const TIME_WIDTH: usize = 26;

/// Display width of the `TYPE` column. `container` is the longest type libpod
/// emits (`network`, `volume`, `image`, `secret`, `pod` are all shorter).
const TYPE_WIDTH: usize = 9;

/// Display width of the `ACTION` column. `health_status` is the longest verb
/// seen on the feed.
const ACTION_WIDTH: usize = 14;

/// The header `events` prints once, before the first event.
///
/// Fixed widths, not content-sized like every other table here: rows arrive over
/// time, so there is nothing to measure up front. `events` was the only stream
/// with no header and no padding at all: three fields joined by single spaces,
/// so `NAME` sat in a different column on every line and nothing named what the
/// fields were.
fn events_header() -> String {
	format!(
		"{:<TIME_WIDTH$} {:<TYPE_WIDTH$} {:<ACTION_WIDTH$} {}",
		"TIME", "TYPE", "ACTION", "NAME"
	)
}

/// Join an event's three fields, tinting the two that carry meaning.
///
/// A `--follow` stream is a wall of near-identical lines; `ACTION` is what
/// distinguishes a `start` from a `die`, and `NAME` is which container it
/// happened to. The type (`container`, `network`) repeats on almost every line
/// and is dimmed so it stops competing.
fn format_event_line(
	time: Option<i64>,
	typ: &str,
	action: &str,
	name: &str,
	colour: bool,
) -> String {
	use crate::ui::{fit_cell, identity_style, paint, Style};
	// `fit_cell` pads *and* escapes. The escaping is not incidental here: an
	// event's actor name comes from outside podup, this line painted it raw, and
	// every other table in the binary has run cells through `sanitize_cell` since
	// the colour work landed. A `\x1b[` in a container name repainted the
	// reader's terminal and desynchronised podup's own resets for every line
	// after it.
	// Dimmed like the type: on a `--follow` wall of lines the seconds are what
	// the eye needs, and the repeated date should not compete with the action.
	let time_cell = fit_cell(&time.map(format_event_time).unwrap_or_default(), TIME_WIDTH);
	let typ_cell = fit_cell(typ, TYPE_WIDTH);
	let action_cell = fit_cell(action, ACTION_WIDTH);
	let name_cell = fit_cell(name, 0);
	let time_out = paint(Style::new().dimmed(), &time_cell, colour);
	let typ_out = paint(Style::new().dimmed(), &typ_cell, colour);
	let action_out = match crate::ui::action_or_status_style(action) {
		Some(style) => paint(style, &action_cell, colour),
		None => action_cell,
	};
	let name_out = paint(
		identity_style(&name_cell),
		&name_cell,
		colour && !name_cell.is_empty(),
	);
	format!("{time_out} {typ_out} {action_out} {name_out}")
		.trim_end()
		.to_string()
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "events_event_colour_tests.rs"]
mod event_colour_tests;

#[cfg(test)]
#[path = "events_event_json_tests.rs"]
mod event_json_tests;

/// Whether an events feed that ended did so because it was asked to.
///
/// The four other streaming commands re-check the container they followed. An
/// events feed follows none, so the discriminator is intent, which the client
/// knows without an API call.
#[cfg(test)]
#[cfg(unix)]
#[path = "events_stream_end_tests.rs"]
mod stream_end_tests;
