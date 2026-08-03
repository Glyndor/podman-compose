//! `events` — stream Podman events scoped to the project (`docker compose
//! events`). Filters the libpod event stream by the `podup.project` label.

use futures_util::StreamExt;
use serde_json::Value;

use crate::error::{ComposeError, Result};
use crate::libpod::{urlencoded, API_PREFIX};

use super::Engine;

/// Options for [`Engine::stream_events`], mirroring `docker compose events`
/// (`--since`, `--until`, `--filter`).
#[derive(Debug, Clone, Default)]
pub struct EventsOptions {
	/// Only events at or after this timestamp/relative time (`--since`).
	pub since: Option<String>,
	/// Only events up to this timestamp/relative time (`--until`).
	pub until: Option<String>,
	/// Extra `KEY=VALUE` event filters (`--filter`, e.g. `event=start`).
	pub filters: Vec<String>,
}

impl Engine {
	/// Stream events for this project's containers. With `json`, each event is
	/// printed as a compact JSON line; otherwise as `TYPE ACTION NAME`.
	///
	/// The feed is unbounded, so it normally ends only when the caller stops it.
	/// Returning at all therefore means the stream was lost, and this returns
	/// `Err` — see [`Engine::stream_events_with_options`] for the bounded case.
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
	/// - **`since` and `until` both set, both already elapsed** — the window
	///   closes on its own, so a clean ending is what was asked for. Returns
	///   `Ok(())`.
	/// - **anything else** — the feed is unbounded and libpod never ends it, so
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
/// the stream to the whole project instead — `events --filter garbage` printed
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
		return serde_json::to_string(value).unwrap_or_default();
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
/// It moved to `crate::timestamp`, which already held the inverse for parsing —
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
/// in one edit — a width that describes a format it no longer holds is the same
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
/// with no header and no padding at all — three fields joined by single spaces,
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
mod tests {
	use super::{build_event_filters, format_event, TIME_WIDTH};
	use serde_json::json;

	#[test]
	fn build_event_filters_scopes_to_project_label() {
		let f = build_event_filters("demo", &[]).unwrap();
		assert_eq!(f, json!({ "label": ["podup.project=demo"] }));
	}

	#[test]
	fn build_event_filters_merges_user_predicates() {
		let f = build_event_filters(
			"demo",
			&[
				"event=start".to_string(),
				"event=die".to_string(),
				"type=container".to_string(),
			],
		)
		.unwrap();
		assert_eq!(
			f,
			json!({
				"label": ["podup.project=demo"],
				"event": ["start", "die"],
				"type": ["container"],
			})
		);
	}

	/// #1081: a predicate with no `=` used to be dropped, so `events --filter
	/// garbage` silently scoped to the whole project and printed everything — a
	/// caller reads that back as "these all matched".
	#[test]
	fn malformed_filter_is_rejected_not_dropped() {
		let err = build_event_filters("demo", &["bogus".to_string()])
			.expect_err("a filter with no `=` must not be silently ignored");
		assert!(format!("{err}").contains("bogus"), "got {err}");
	}

	#[test]
	fn formats_docker_compat_shape() {
		let ev = json!({
			"Type": "container",
			"Action": "start",
			"Actor": { "Attributes": { "name": "web-1" } },
			"time": 0,
		});
		// Columns are fixed-width now (#1248): TIME leads, `container` fills TYPE
		// exactly, `start` is padded out to ACTION, and NAME is the trailing raw
		// column.
		//
		// The TIME cell is asserted by width rather than by value: it renders
		// the reader's wall clock, so a fixed string would pass on a machine at
		// -05:00 and fail on a runner at UTC. `crate::timestamp` pins what goes
		// in the cell; this pins that the columns after it land where the header
		// says.
		let out = format_event(&ev, false);
		assert_eq!(
			&out[TIME_WIDTH..],
			" container start          web-1",
			"columns after TIME drifted: {out:?}"
		);
	}

	#[test]
	fn formats_libpod_native_shape() {
		let ev = json!({ "Type": "container", "status": "die", "id": "abc123", "time": 0 });
		let out = format_event(&ev, false);
		assert_eq!(
			&out[TIME_WIDTH..],
			" container die            abc123",
			"columns after TIME drifted: {out:?}"
		);
	}

	#[test]
	fn json_mode_emits_raw_object() {
		let ev = json!({ "Type": "container", "Action": "start" });
		let out = format_event(&ev, true);
		assert!(out.contains("\"Type\":\"container\""));
		assert!(out.contains("\"Action\":\"start\""));
	}
}

#[cfg(test)]
mod event_colour_tests {
	use super::{format_event_line, TIME_WIDTH};

	// The instant-by-instant pinning that used to live here moved to
	// `crate::timestamp` with the arithmetic itself. Keeping a copy next to the
	// caller would be two places to update and one of them would rot.

	/// An event with no `time` renders the column blank rather than inventing a
	/// value. A timestamp that looks real and is not is worse than none.
	#[test]
	fn an_event_without_a_time_leaves_the_column_empty() {
		let line = format_event_line(None, "container", "start", "web-1", false);
		assert!(
			line.starts_with(&" ".repeat(TIME_WIDTH)),
			"expected a blank time cell, got: {line:?}"
		);
	}

	/// Without a colour sink the line is byte-identical to what it always was,
	/// so `--json`, a pipe and the output contract are untouched.
	#[test]
	fn plain_output_carries_no_escapes() {
		let out = format_event_line(Some(0), "container", "start", "proj-web-1", false);
		assert!(!out.contains('\u{1b}'), "{out:?}");
		assert_eq!(
			&out[TIME_WIDTH..],
			" container start          proj-web-1",
			"{out:?}"
		);
	}

	/// The three fields line up under the header, whatever their lengths, so
	/// NAME does not move column on every line the way it used to.
	#[test]
	fn columns_align_under_the_header() {
		let header = super::events_header();
		let short = format_event_line(None, "pod", "die", "a", false);
		let long = format_event_line(None, "container", "health_status", "b", false);
		let name_col = |line: &str| line.rfind(' ').map(|i| i + 1);
		assert_eq!(
			name_col(&header),
			name_col(&short),
			"header {header:?} vs {short:?}"
		);
		assert_eq!(
			name_col(&header),
			name_col(&long),
			"header {header:?} vs {long:?}"
		);
	}

	/// An actor name comes from outside podup. This line painted it raw while
	/// every other table escaped its cells, so an escape sequence in a container
	/// name repainted the reader's terminal.
	#[test]
	fn an_actor_name_cannot_drive_the_terminal() {
		let out = format_event_line(None, "container", "start", "evil\u{1b}[31m\u{7}name", false);
		assert!(!out.contains('\u{1b}'), "{out:?}");
		assert!(!out.contains('\u{7}'), "{out:?}");
		assert!(out.contains("name"), "{out:?}");
	}

	/// The two fields that distinguish one line from the next carry colour; the
	/// type, which repeats on nearly every line, is dimmed rather than absent.
	#[test]
	fn action_and_name_are_tinted_apart() {
		let died = format_event_line(None, "container", "die", "proj-web-1", true);
		let started = format_event_line(None, "container", "start", "proj-web-1", true);
		assert_ne!(
			died,
			started.replace("start", "die"),
			"die and start must differ by more than the verb"
		);
	}

	/// An event with no container name must not emit a stray colour reset.
	#[test]
	fn an_empty_name_is_not_painted() {
		let out = format_event_line(None, "network", "create", "", true);
		assert!(
			out.ends_with("create\u{1b}[0m") || !out.ends_with("\u{1b}[0m "),
			"{out:?}"
		);
	}
}

/// Whether an events feed that ended did so because it was asked to.
///
/// The four other streaming commands re-check the container they followed. An
/// events feed follows none, so the discriminator is intent — which the client
/// knows without an API call.
#[cfg(test)]
#[cfg(unix)]
mod stream_end_tests {
	use crate::engine::fake_podman::{self, FakeReply};
	use crate::engine::{Engine, EventsOptions};

	fn engine(fake: &fake_podman::FakePodman) -> Engine {
		Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir())
	}

	/// One event, then the body ends the way the server chose.
	fn fake(reply: fn() -> FakeReply) -> fake_podman::FakePodman {
		fake_podman::start_replying(move |_method, _target| reply())
	}

	fn one_event() -> Vec<String> {
		vec![r#"{"Type":"container","Action":"start","id":"abc"}"#.to_string()]
	}

	/// Both ends of an elapsed window, which is the only form libpod closes.
	fn bounded() -> EventsOptions {
		EventsOptions {
			since: Some("2026-01-01T00:00:00Z".to_string()),
			until: Some("2026-01-01T01:00:00Z".to_string()),
			..Default::default()
		}
	}

	#[tokio::test]
	async fn an_unbounded_feed_that_ends_cleanly_is_still_a_failure() {
		// The case no error-shaped check could catch: the server closed the body
		// properly, so the parser reports a clean end, and this used to exit 0.
		let fake = fake(|| FakeReply::ChunkedEnd(one_event()));
		let err = engine(&fake)
			.stream_events_with_options(false, &EventsOptions::default())
			.await
			.expect_err("only the client ends an unbounded feed, so any end is unexpected");
		assert!(
			matches!(err, crate::error::ComposeError::StreamTruncated(_)),
			"expected the intent verdict, got {err:?}"
		);
	}

	#[tokio::test]
	async fn an_unbounded_feed_cut_mid_body_is_a_failure() {
		let fake = fake(|| FakeReply::ChunkedTruncated(one_event()));
		let err = engine(&fake)
			.stream_events_with_options(false, &EventsOptions::default())
			.await
			.expect_err("a severed unbounded feed is a failure too");
		assert!(
			matches!(err, crate::error::ComposeError::Podman(_)),
			"the transport error must survive so the operator sees the cause, got {err:?}"
		);
	}

	#[tokio::test]
	async fn a_bounded_feed_that_ends_is_success() {
		// `--until` is the client saying the window closes on its own, so the end
		// is what was asked for. Measured on 5.4.2: an already-elapsed window does
		// end the feed cleanly.
		let fake = fake(|| FakeReply::ChunkedEnd(one_event()));
		engine(&fake)
			.stream_events_with_options(false, &bounded())
			.await
			.expect("a bounded feed reaching the end of its window succeeded");
	}

	/// `--until` alone does not bound anything: measured against 5.4.2, libpod
	/// leaves the feed open without a `since` to pair it with. Treating it as
	/// bounded would call an unbounded feed bounded and hand back a success.
	#[tokio::test]
	async fn until_without_since_is_not_a_bounded_feed() {
		let fake = fake(|| FakeReply::ChunkedEnd(one_event()));
		let opts = EventsOptions {
			until: Some("2026-01-01T00:00:00Z".to_string()),
			..Default::default()
		};
		let err = engine(&fake)
			.stream_events_with_options(false, &opts)
			.await
			.expect_err("until alone leaves the feed unbounded, so any end is unexpected");
		assert!(
			matches!(err, crate::error::ComposeError::StreamTruncated(_)),
			"expected the unbounded verdict, got {err:?}"
		);
	}

	#[tokio::test]
	async fn a_bounded_feed_cut_mid_body_is_a_failure() {
		// Intent says whether an *ending* was expected. It cannot make a severed
		// socket expected, and this is the case that matters most: `--until` with
		// `--format json` is the scriptable form, so swallowing the error here
		// would truncate a window and report success on exactly the path a script
		// trusts, while the interactive unbounded form kept the strict check.
		// That inverts #1104 rather than completing it.
		let fake = fake(|| FakeReply::ChunkedTruncated(one_event()));
		let err = engine(&fake)
			.stream_events_with_options(false, &bounded())
			.await
			.expect_err("a severed window is a failed read, bounded or not");
		assert!(
			matches!(err, crate::error::ComposeError::Podman(_)),
			"the transport error must survive so the operator sees the cause, got {err:?}"
		);
	}
}
