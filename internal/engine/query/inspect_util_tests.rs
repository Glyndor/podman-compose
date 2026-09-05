/// A process can name itself, and `top` prints that name. Without escaping,
/// a container process called `\x1b[31mevil` repaints the reader's terminal
/// — the one table in podup that formatted by hand rather than through
/// `fit_cell`, which has sanitized since it was written.
#[test]
fn top_escapes_control_characters_from_process_argv() {
	let titles = vec!["PID".to_string(), "COMMAND".to_string()];
	let processes = vec![vec!["1".to_string(), "\u{1b}[31mevil".to_string()]];
	let out = super::process_table(&titles, &processes).unwrap().render();
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
	dedup_preserving_order, is_running_status, parse_port_proto, select_replica, split_repo_tag,
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
	let out = dedup_preserving_order(&["web".into(), "db".into(), "web".into(), "cache".into()]);
	assert_eq!(out, vec!["web", "db", "cache"]);
}

#[test]
fn top_pads_columns_to_the_widest_cell() {
	let titles = vec!["PID".to_string(), "CMD".to_string()];
	let processes = vec![
		vec!["1".to_string(), "bash".to_string()],
		vec!["12345".to_string(), "node".to_string()],
	];
	let lines = super::process_table(&titles, &processes).unwrap().render();
	assert_eq!(lines.len(), 3);
	// The invariant is that the second column starts at one offset on every
	// line, header included — not any particular prefix. Asserting a literal
	// `"1      "` pinned the old hand-rolled aligner's two-space join, so it
	// failed when `top` moved onto the shared table for no reason a reader of
	// `top` would care about.
	let second_col = |line: &str| {
		line.find(|c: char| !c.is_whitespace()).map(|_| {
			let first = line.split_whitespace().next().unwrap_or("");
			line.find(first).unwrap_or(0)
				+ first.len()
				+ line[line.find(first).unwrap_or(0) + first.len()..]
					.chars()
					.take_while(|c| *c == ' ')
					.count()
		})
	};
	assert_eq!(second_col(&lines[0]), second_col(&lines[1]));
	assert_eq!(second_col(&lines[0]), second_col(&lines[2]));
	// The widest cell ("12345") is what sets that offset.
	assert_eq!(second_col(&lines[0]), Some("12345".len() + 1));
	// No tabs in the aligned output.
	assert!(lines.iter().all(|l| !l.contains('\t')));
}

/// `top` dims the bookkeeping columns so the command line is what the eye
/// lands on. Selected by title, so libpod reordering or renaming a column
/// cannot silently dim the wrong one.
#[test]
fn top_dims_the_bookkeeping_columns_only() {
	let titles: Vec<String> = ["UID", "PID", "PPID", "C", "STIME", "TTY", "TIME", "CMD"]
		.iter()
		.map(|s| (*s).to_string())
		.collect();
	let dim = super::top_dim_columns(&titles);
	// PID (1) and CMD (7) are the answer, everything else is scaffolding.
	assert_eq!(dim, vec![0, 2, 3, 4, 5, 6]);
}

/// A title libpod might add in future is left at normal weight rather than
/// dimmed on a guess.
#[test]
fn top_leaves_an_unknown_column_alone() {
	let titles = vec!["PID".to_string(), "RSS".to_string(), "CMD".to_string()];
	assert!(super::top_dim_columns(&titles).is_empty());
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

/// The CREATED column of `ps` and `images` reads as `N units ago`, the same
/// wording docker compose uses. The boundaries below are exactly the ones the
/// issue pinned: 0 s, 1 s, 59 s, 60 s, 59 min, 60 min, 23 h, 24 h, plus a
/// 1-vs-2 check at every unit so the singular/plural rule is also pinned.
#[test]
fn humanize_age_covers_the_pinned_boundaries() {
	assert_eq!(super::humanize_age(0), "Less than a second ago");
	assert_eq!(super::humanize_age(-3), "Less than a second ago");
	assert_eq!(super::humanize_age(1), "1 second ago");
	assert_eq!(super::humanize_age(2), "2 seconds ago");
	assert_eq!(super::humanize_age(59), "59 seconds ago");
	assert_eq!(super::humanize_age(60), "1 minute ago");
	assert_eq!(super::humanize_age(61), "1 minute ago");
	assert_eq!(super::humanize_age(2 * 60), "2 minutes ago");
	assert_eq!(super::humanize_age(59 * 60), "59 minutes ago");
	assert_eq!(super::humanize_age(60 * 60), "1 hour ago");
	assert_eq!(super::humanize_age(60 * 60 + 1), "1 hour ago");
	assert_eq!(super::humanize_age(2 * 60 * 60), "2 hours ago");
	assert_eq!(super::humanize_age(23 * 60 * 60), "23 hours ago");
	assert_eq!(super::humanize_age(24 * 60 * 60), "1 day ago");
	assert_eq!(super::humanize_age(24 * 60 * 60 + 1), "1 day ago");
	assert_eq!(super::humanize_age(2 * 24 * 60 * 60), "2 days ago");
}

/// The snapshot the issue was filed against: `6s` and `3m 19s` read as elapsed
/// timers, not as points in the past. The fix uses the same wording for both
/// columns of both commands.
#[test]
fn humanize_age_matches_the_docker_compose_wording() {
	assert_eq!(super::humanize_age(6), "6 seconds ago");
	assert_eq!(super::humanize_age(3 * 60 + 19), "3 minutes ago");
}
