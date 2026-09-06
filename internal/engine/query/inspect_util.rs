//! Pure helpers for `inspect` and the read-only query commands: replica/port
//! selection, image-ref splitting, process-table formatting, and the
//! shared "N units ago" renderer used by `ps` and `images`. Kept free of the
//! live Podman client so each is unit-tested in isolation.

use crate::error::{ComposeError, Result};

/// Render an elapsed span as `N units ago`, the same wording `docker compose`
/// uses for `ps` and `images` CREATED columns.
///
/// Largest unit only (so a 90-second row reads `1 minute ago`, not
/// `1 minute 30 seconds ago`), singular at exactly one (`1 day ago`, not
/// `1 days ago`), and `Less than a second ago` under one second, since an
/// elapsed timer like `6s` reads as *how long since*, not *how long ago*,
/// and the `_ago` wording is what fixes that.
///
/// `seconds` is clamped to zero on a negative argument (clock skew between
/// this process and libpod is not a future age). Pure so the boundaries
/// (`0 s`, `1 s`, `59 s`, `60 s`, `59 min`, `60 min`, `23 h`, `24 h`) are
/// pinned without a live socket.
pub(super) fn humanize_age(seconds: i64) -> String {
	if seconds < 1 {
		return "Less than a second ago".to_string();
	}
	let s = seconds as u64;
	if s < 60 {
		return format_age(s, "second");
	}
	if s < 3600 {
		return format_age(s / 60, "minute");
	}
	if s < 86_400 {
		return format_age(s / 3600, "hour");
	}
	format_age(s / 86_400, "day")
}

/// `1 second ago` at one, `N seconds ago` afterwards (same shape for
/// minute/hour/day). Kept tiny so the singular/plural rule is a one-line
/// change instead of a duplicated format string.
fn format_age(n: u64, unit: &str) -> String {
	if n == 1 {
		format!("1 {unit} ago")
	} else {
		format!("{n} {unit}s ago")
	}
}

/// Pick a service's target replica container from its live container names.
///
/// Names are ordered by their trailing `-N` suffix (numerically, so `svc-10`
/// sorts after `svc-2`); an unsuffixed single-replica name sorts first. `index`
/// is the 1-based `--index`; `None` selects the first replica. Pure so the
/// indexing is unit-tested without a live Podman socket.
pub(super) fn select_replica(
	mut names: Vec<String>,
	service_name: &str,
	index: Option<u32>,
) -> Result<String> {
	names.sort_by_key(|n| {
		n.rsplit_once('-')
			.and_then(|(_, suffix)| suffix.parse::<u64>().ok())
			.unwrap_or(0)
	});
	match index {
		Some(i) => {
			// `--index` is 1-based; `0` is invalid, not "first replica".
			let idx = (i as usize).checked_sub(1).ok_or_else(|| {
				ComposeError::ServiceNotFound(format!(
					"{service_name} (replica index {i}: indexes are 1-based)"
				))
			})?;
			names.get(idx).cloned().ok_or_else(|| {
				ComposeError::ServiceNotFound(format!("{service_name} (replica index {i})"))
			})
		}
		None => names
			.into_iter()
			.next()
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into())),
	}
}

/// Resolve the `(port, proto)` for `port` from a `PORT` or `PORT/proto` argument,
/// the `/proto` suffix overriding the `--protocol` flag, matching
/// `docker compose port`. Pure so the parsing is unit-tested.
///
/// The private port is parsed strictly like docker's port spec: only a canonical
/// decimal `1..=65535` is accepted, so a leading `+`/`-`, leading zeros
/// (`080`), surrounding whitespace, or any trailing junk (`80/tcp/extra`) are
/// rejected rather than silently coerced. The protocol is validated to
/// `tcp`/`udp` (case-insensitive) and normalised to lowercase so it matches the
/// lowercase keys Podman reports; an unknown or empty protocol errors clearly
/// instead of yielding an empty, exit-0 "no mapping" result.
pub(super) fn parse_port_proto(
	private_port: &str,
	proto_flag: &str,
) -> Result<(u16, &'static str)> {
	let (port_str, proto_str) = match private_port.split_once('/') {
		Some((p, pr)) => (p, pr),
		None => (private_port, proto_flag),
	};
	let port = parse_strict_port(port_str, private_port)?;
	let proto = normalise_proto(proto_str)?;
	Ok((port, proto))
}

/// Parse a private port strictly: a canonical decimal in `1..=65535`. Rejects an
/// empty string, a leading sign (`+80`/`-80`), leading zeros (`080`), embedded
/// whitespace, and any non-digit (so trailing junk cannot slip through).
fn parse_strict_port(port_str: &str, private_port: &str) -> Result<u16> {
	let invalid = || {
		ComposeError::InvalidPort(format!(
			"port '{private_port}' is not a valid PORT or PORT/proto"
		))
	};
	if port_str.is_empty() || !port_str.bytes().all(|b| b.is_ascii_digit()) {
		return Err(invalid());
	}
	// Reject non-canonical leading zeros (e.g. `080`); a bare `0` is caught below.
	if port_str.len() > 1 && port_str.starts_with('0') {
		return Err(invalid());
	}
	let port: u16 = port_str.parse().map_err(|_| invalid())?;
	if port == 0 {
		return Err(invalid());
	}
	Ok(port)
}

/// Validate and normalise a protocol to `tcp`/`udp` (case-insensitive). Returns a
/// lowercase `&'static str` so it matches the keys Podman reports; anything else
/// (including an empty string) is a clear error.
fn normalise_proto(proto: &str) -> Result<&'static str> {
	if proto.eq_ignore_ascii_case("tcp") {
		Ok("tcp")
	} else if proto.eq_ignore_ascii_case("udp") {
		Ok("udp")
	} else {
		Err(ComposeError::InvalidPort(format!(
			"protocol '{proto}' is not valid (expected 'tcp' or 'udp')"
		)))
	}
}

/// Deduplicate a list of strings, preserving first-seen order. Used so `top web
/// web` queries and prints each service once, matching `docker compose top`.
pub(super) fn dedup_preserving_order(items: &[String]) -> Vec<String> {
	let mut seen = std::collections::HashSet::new();
	items
		.iter()
		.filter(|s| seen.insert(s.as_str()))
		.cloned()
		.collect()
}

/// Whether a libpod container `Status` string denotes a running container.
/// `docker compose attach` only attaches to a running container; anything else
/// (exited, created, paused, empty/unknown) must fail closed.
pub(super) fn is_running_status(status: &str) -> bool {
	status.eq_ignore_ascii_case("running")
}

/// Split an image reference into `(repository, tag)` for the `images` table.
///
/// A trailing `:tag` is only a tag when the segment after it has no `/` (so a
/// `registry:port/name` host is not mis-split), mirroring the guard in
/// `export.rs`. A `name@sha256:...` digest reference has no tag, shown as
/// `<none>` like docker, and the long digest never bloats the TAG column.
pub(super) fn split_repo_tag(image_ref: &str) -> (String, String) {
	if let Some((repo, _digest)) = image_ref.split_once('@') {
		return (repo.to_string(), "<none>".to_string());
	}
	match image_ref.rsplit_once(':') {
		Some((repo, tag)) if !tag.contains('/') => (repo.to_string(), tag.to_string()),
		_ => (image_ref.to_string(), "latest".to_string()),
	}
}

/// Column titles that are process bookkeeping rather than the answer.
///
/// Matched by title, not by index: libpod chooses the column set, so a
/// positional list would silently dim the wrong things if that set ever
/// changed. PID and CMD are what a reader of `top` is looking for; an
/// unrecognised title is left alone.
const TOP_SCAFFOLDING: [&str; 6] = ["UID", "PPID", "C", "STIME", "TTY", "TIME"];

/// Which of `titles` are bookkeeping and should be dimmed.
///
/// Pure and separate from the table so the choice is testable: a dimming that
/// selects nothing renders identically to no dimming at all, so without this the
/// control could be deleted with the suite staying green.
pub(super) fn top_dim_columns(titles: &[String]) -> Vec<usize> {
	titles
		.iter()
		.enumerate()
		.filter(|(_, t)| TOP_SCAFFOLDING.contains(&t.as_str()))
		.map(|(i, _)| i)
		.collect()
}

/// Build the table for one container's process list, or `None` when libpod sent
/// no titles.
///
/// On `ui::Table` rather than a hand-rolled aligner: cells are escaped and
/// columns sized in one place, so `top` stops being a third layout dialect that
/// has to be fixed separately every time. The escaping is not incidental: these
/// cells hold a process `argv` read out of a container, which is
/// attacker-controlled, and a process can name itself.
///
/// Pure (returns the table instead of printing it) so both properties stay
/// testable without a live container.
pub(super) fn process_table(
	titles: &[String],
	processes: &[Vec<String>],
) -> Option<crate::ui::Table> {
	if titles.is_empty() {
		return None;
	}
	let headers: Vec<&str> = titles.iter().map(String::as_str).collect();
	let mut table = crate::ui::Table::new(&headers).dim_cols(&top_dim_columns(titles));
	for row in processes {
		table.push(row.clone());
	}
	Some(table)
}

#[cfg(test)]
#[path = "inspect_util_tests.rs"]
mod tests;
