//! Pure helpers for `inspect`: replica/port selection, image-ref
//! splitting, and process-table formatting. Kept free of the live Podman
//! client so each is unit-tested in isolation.

use crate::error::{ComposeError, Result};

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
/// the `/proto` suffix overriding the `--protocol` flag — matching
/// `docker compose port`. Pure so the parsing is unit-tested.
///
/// The private port is parsed strictly like docker's port spec: only a canonical
/// decimal `1..=65535` is accepted, so a leading `+`/`-`, leading zeros
/// (`080`), surrounding whitespace, or any trailing junk (`80/tcp/extra`) are
/// rejected rather than silently coerced. The protocol is validated to
/// `tcp`/`udp` (case-insensitive) and normalised to lowercase so it matches the
/// lowercase keys Podman reports — an unknown or empty protocol errors clearly
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

/// Align a `top` table (the title row followed by process rows) into
/// space-padded columns sized to the widest cell, returning one rendered line
/// per input row (titles first). All but the last column are left-padded; the
/// last is left ragged to avoid trailing whitespace.
pub(super) fn align_top_columns(titles: &[String], processes: &[Vec<String>]) -> Vec<String> {
	// Escaped before anything is measured. These cells hold a process `argv`
	// read out of a container, which is attacker-controlled: without this a
	// process can name itself with ANSI and repaint the reader's terminal. Every
	// other table in podup goes through `fit_cell`, which sanitizes; `top`
	// formats by hand and was the one that did not. Escaping first also keeps
	// the width honest, since an escaped control character is wider than the
	// byte it replaces.
	let sanitize = |row: &[String]| -> Vec<String> {
		row.iter().map(|c| crate::ui::sanitize_cell(c)).collect()
	};
	let mut rows: Vec<Vec<String>> = Vec::with_capacity(processes.len() + 1);
	if !titles.is_empty() {
		rows.push(sanitize(titles));
	}
	for p in processes {
		rows.push(sanitize(p));
	}
	let col_count = rows.iter().map(|r| r.len()).max().unwrap_or(0);
	let mut widths = vec![0usize; col_count];
	for r in &rows {
		for (i, cell) in r.iter().enumerate() {
			widths[i] = widths[i].max(cell.chars().count());
		}
	}
	rows.iter()
		.map(|r| {
			r.iter()
				.enumerate()
				.map(|(i, cell)| {
					if i + 1 == r.len() {
						cell.clone()
					} else {
						format!("{cell:<width$}", width = widths[i])
					}
				})
				.collect::<Vec<_>>()
				.join("  ")
		})
		.collect()
}

#[cfg(test)]
mod tests {
	/// A process can name itself, and `top` prints that name. Without escaping,
	/// a container process called `\x1b[31mevil` repaints the reader's terminal
	/// — the one table in podup that formatted by hand rather than through
	/// `fit_cell`, which has sanitized since it was written.
	#[test]
	fn top_escapes_control_characters_from_process_argv() {
		let titles = vec!["PID".to_string(), "COMMAND".to_string()];
		let processes = vec![vec!["1".to_string(), "\u{1b}[31mevil".to_string()]];
		let out = super::align_top_columns(&titles, &processes);
		assert!(
			!out.iter().any(|line| line.contains('\u{1b}')),
			"no raw escape may reach the terminal: {out:?}"
		);
		assert!(
			out[1].contains("\\u{1b}") || out[1].contains("\\x1b") || out[1].contains("\\e"),
			"the sequence must survive as visible text, not vanish: {out:?}"
		);
	}

	use super::{
		align_top_columns, dedup_preserving_order, is_running_status, parse_port_proto,
		select_replica, split_repo_tag,
	};

	#[test]
	fn select_replica_none_picks_first_by_suffix() {
		// Live names come back in arbitrary API order; the first replica is the
		// lowest-suffixed one regardless.
		let names = vec![
			"proj-web-3".into(),
			"proj-web-1".into(),
			"proj-web-2".into(),
		];
		assert_eq!(select_replica(names, "web", None).unwrap(), "proj-web-1");
	}

	#[test]
	fn select_replica_orders_suffix_numerically() {
		// `-10` must sort after `-2`, not lexicographically before it.
		let names = vec![
			"proj-web-10".into(),
			"proj-web-2".into(),
			"proj-web-1".into(),
		];
		assert_eq!(
			select_replica(names, "web", Some(3)).unwrap(),
			"proj-web-10"
		);
	}

	#[test]
	fn select_replica_index_targets_nth() {
		let names = vec!["proj-web-1".into(), "proj-web-2".into()];
		assert_eq!(
			select_replica(names.clone(), "web", Some(2)).unwrap(),
			"proj-web-2"
		);
	}

	#[test]
	fn select_replica_unsuffixed_single() {
		let names = vec!["proj-web".into()];
		assert_eq!(select_replica(names, "web", None).unwrap(), "proj-web");
	}

	#[test]
	fn select_replica_rejects_index_zero_and_out_of_range() {
		let names = vec!["proj-web-1".into(), "proj-web-2".into()];
		assert!(select_replica(names.clone(), "web", Some(0)).is_err());
		assert!(select_replica(names, "web", Some(5)).is_err());
	}

	#[test]
	fn select_replica_empty_is_not_found() {
		assert!(select_replica(vec![], "web", None).is_err());
	}

	#[test]
	fn split_repo_tag_plain_name_and_tag() {
		assert_eq!(
			split_repo_tag("nginx:1.25"),
			("nginx".into(), "1.25".into())
		);
		assert_eq!(split_repo_tag("nginx"), ("nginx".into(), "latest".into()));
	}

	#[test]
	fn split_repo_tag_registry_with_port_is_not_a_tag() {
		// The ':' belongs to the registry host:port, not a tag.
		assert_eq!(
			split_repo_tag("registry:5000/team/app"),
			("registry:5000/team/app".into(), "latest".into())
		);
		assert_eq!(
			split_repo_tag("registry:5000/team/app:v2"),
			("registry:5000/team/app".into(), "v2".into())
		);
	}

	#[test]
	fn split_repo_tag_digest_has_no_tag() {
		let (repo, tag) = split_repo_tag(
			"docker.io/library/alpine@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
		);
		assert_eq!(repo, "docker.io/library/alpine");
		assert_eq!(tag, "<none>");
	}

	#[test]
	fn dedup_preserving_order_keeps_first_occurrence() {
		let out =
			dedup_preserving_order(&["web".into(), "db".into(), "web".into(), "cache".into()]);
		assert_eq!(out, vec!["web", "db", "cache"]);
	}

	#[test]
	fn align_top_columns_pads_to_widest_cell() {
		let titles = vec!["PID".to_string(), "CMD".to_string()];
		let processes = vec![
			vec!["1".to_string(), "bash".to_string()],
			vec!["12345".to_string(), "node".to_string()],
		];
		let lines = align_top_columns(&titles, &processes);
		assert_eq!(lines.len(), 3);
		// First column is padded to the widest value ("12345" = 5 chars).
		assert!(lines[0].starts_with("PID  "));
		assert!(lines[1].starts_with("1      "));
		// No tabs in the aligned output.
		assert!(lines.iter().all(|l| !l.contains('\t')));
	}

	#[test]
	fn bare_port_uses_flag_proto() {
		assert_eq!(parse_port_proto("80", "tcp").unwrap(), (80, "tcp"));
	}

	#[test]
	fn suffix_overrides_flag_proto() {
		assert_eq!(parse_port_proto("53/udp", "tcp").unwrap(), (53, "udp"));
	}

	#[test]
	fn non_numeric_port_is_rejected() {
		assert!(parse_port_proto("http", "tcp").is_err());
		assert!(parse_port_proto("abc/tcp", "tcp").is_err());
	}

	#[test]
	fn protocol_flag_is_case_insensitive() {
		assert_eq!(parse_port_proto("80", "TCP").unwrap(), (80, "tcp"));
		assert_eq!(parse_port_proto("80", "Udp").unwrap(), (80, "udp"));
		assert_eq!(parse_port_proto("53/UDP", "tcp").unwrap(), (53, "udp"));
	}

	#[test]
	fn unknown_or_empty_protocol_is_rejected() {
		// Previously these silently produced an empty, exit-0 "no mapping".
		assert!(parse_port_proto("80", "sctp").is_err());
		assert!(parse_port_proto("80", "").is_err());
		assert!(parse_port_proto("80/sctp", "tcp").is_err());
	}

	#[test]
	fn non_canonical_private_port_is_rejected() {
		// Leading sign, leading zeros, whitespace, and trailing junk must all
		// error rather than being coerced or silently mis-handled.
		assert!(parse_port_proto("+80", "tcp").is_err());
		assert!(parse_port_proto("-80", "tcp").is_err());
		assert!(parse_port_proto("080", "tcp").is_err());
		assert!(parse_port_proto(" 80", "tcp").is_err());
		assert!(parse_port_proto("80 ", "tcp").is_err());
		assert!(parse_port_proto("80/tcp/extra", "tcp").is_err());
		assert!(parse_port_proto("0", "tcp").is_err());
		assert!(parse_port_proto("65536", "tcp").is_err());
	}

	#[test]
	fn dedup_keeps_first_occurrence_order() {
		let input = ["web".to_string(), "web".to_string(), "db".to_string()];
		assert_eq!(dedup_preserving_order(&input), vec!["web", "db"]);
		let input = [
			"a".to_string(),
			"b".to_string(),
			"a".to_string(),
			"c".to_string(),
			"b".to_string(),
		];
		assert_eq!(dedup_preserving_order(&input), vec!["a", "b", "c"]);
	}

	#[test]
	fn running_status_detected_case_insensitively() {
		assert!(is_running_status("running"));
		assert!(is_running_status("Running"));
		// Anything else is not attachable.
		assert!(!is_running_status("exited"));
		assert!(!is_running_status("created"));
		assert!(!is_running_status("paused"));
		assert!(!is_running_status(""));
	}
}
