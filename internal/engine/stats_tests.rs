//! Unit tests for `stats` — split out to keep the module inside the source line
//! limit, following the same `tests.rs` split used by `autostart` and the libpod
//! client.

use super::*;

/// A representative row: values wide enough that a column mis-sized by a couple
/// of characters shows up, rather than a zeroed struct where every cell happens
/// to be short.
fn sample_stat() -> ContainerStat {
	let mut network = HashMap::new();
	network.insert("eth0".to_string(), NetStat { rx: 1024, tx: 2048 });
	ContainerStat {
		name: "proj-web-1".into(),
		cpu: 12.5,
		mem_usage: 1024 * 1024,
		mem_limit: 60 * 1024 * 1024 * 1024,
		mem_perc: 0.06,
		block_in: 4096,
		block_out: 8192,
		pids: 3,
		network,
	}
}

#[test]
fn format_bytes_scales_units() {
	assert_eq!(format_bytes(512), "512B");
	assert_eq!(format_bytes(1024), "1.0KiB");
	assert_eq!(format_bytes(1536), "1.5KiB");
	assert_eq!(format_bytes(1024 * 1024), "1.0MiB");
	assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0GiB");
}

#[test]
fn format_row_sums_network_and_shows_name() {
	let mut network = HashMap::new();
	network.insert("eth0".to_string(), NetStat { rx: 1024, tx: 2048 });
	let s = ContainerStat {
		name: "proj-web".into(),
		cpu: 12.5,
		mem_usage: 1024 * 1024,
		mem_limit: 1024 * 1024 * 1024,
		mem_perc: 0.1,
		block_in: 0,
		block_out: 0,
		pids: 3,
		network,
	};
	let row = format_row_with(&s, false, false);
	assert!(row.contains("proj-web"));
	assert!(row.contains("12.50%"));
	assert!(row.contains("1.0MiB"));
	assert!(row.contains('3'));
}

#[test]
fn truncate_name_shortens_long_names_with_ellipsis() {
	let short = "proj-web-1";
	assert_eq!(truncate_name(short, false), short);
	// Exactly the column width is kept verbatim.
	let exact = "a".repeat(NAME_WIDTH);
	assert_eq!(truncate_name(&exact, false), exact);
	// One over the width is truncated to width-1 chars plus an ellipsis.
	let long = "a".repeat(NAME_WIDTH + 10);
	let cut = truncate_name(&long, false);
	assert_eq!(cut.chars().count(), NAME_WIDTH);
	assert!(cut.ends_with('…'));
	// `--no-trunc` keeps the full name regardless of length.
	assert_eq!(truncate_name(&long, true), long);
}

#[test]
fn format_row_truncates_long_name_but_no_trunc_keeps_it() {
	let s = ContainerStat {
		name: "really-long-project-web-container-name-1".into(),
		..Default::default()
	};
	// Default: the long name is truncated (ellipsis present, full name gone).
	let row = format_row_with(&s, false, false);
	assert!(row.contains('…'));
	assert!(!row.contains(&s.name));
	// `--no-trunc`: the full name survives intact.
	let row_full = format_row_with(&s, true, false);
	assert!(row_full.contains(&s.name));
}

#[test]
fn stat_json_row_emits_numeric_fields() {
	let mut network = HashMap::new();
	network.insert("eth0".to_string(), NetStat { rx: 100, tx: 200 });
	let s = ContainerStat {
		name: "proj-db-1".into(),
		cpu: 5.5,
		mem_usage: 2048,
		mem_limit: 4096,
		mem_perc: 50.0,
		block_in: 1,
		block_out: 2,
		pids: 7,
		network,
	};
	let row = stat_json_row(&s);
	assert_eq!(row["Name"], "proj-db-1");
	assert_eq!(row["CPUPerc"], 5.5);
	assert_eq!(row["MemUsage"], 2048);
	assert_eq!(row["MemLimit"], 4096);
	assert_eq!(row["NetInput"], 100);
	assert_eq!(row["NetOutput"], 200);
	assert_eq!(row["PIDs"], 7);
}

#[test]
fn frame_rows_keeps_running_and_adds_stopped_zeros_sorted() {
	let report = StatsReport {
		stats: vec![
			ContainerStat {
				name: "proj-web-1".into(),
				cpu: 1.0,
				..Default::default()
			},
			// Not in `running` → must be dropped (stale daemon sample).
			ContainerStat {
				name: "other".into(),
				..Default::default()
			},
		],
	};
	let mut running = HashSet::new();
	running.insert("proj-web-1".to_string());
	let stopped = vec!["proj-db-1".to_string()];
	let rows = frame_rows(&report, &running, &stopped);
	// Sorted: db (stopped) before web (running); the unrelated sample is gone.
	assert_eq!(rows.len(), 2);
	assert_eq!(rows[0].name, "proj-db-1");
	assert_eq!(rows[0].cpu, 0.0);
	assert_eq!(rows[1].name, "proj-web-1");
	assert_eq!(rows[1].cpu, 1.0);
}

#[test]
fn frame_rows_without_all_has_no_stopped_rows() {
	let report = StatsReport::default();
	let running = HashSet::new();
	// `--all` off → caller passes an empty `stopped` slice → no rows.
	let rows = frame_rows(&report, &running, &[]);
	assert!(rows.is_empty());
}

#[test]
fn stat_tolerates_null_network() {
	// libpod sends `"Network": null` for a container with no interfaces; it
	// must deserialize to an empty map rather than erroring.
	let json = r#"{"Name":"proj-web","CPU":1.0,"Network":null}"#;
	let stat: ContainerStat = serde_json::from_str(json).unwrap();
	assert_eq!(stat.name, "proj-web");
	assert!(stat.network.is_empty());
}

#[test]
fn stat_tolerates_missing_network() {
	let json = r#"{"Name":"proj-web"}"#;
	let stat: ContainerStat = serde_json::from_str(json).unwrap();
	assert!(stat.network.is_empty());
}

#[test]
fn containers_query_repeats_param_per_container() {
	// libpod wants `containers` repeated, not comma-joined — a comma-joined
	// list is read as a single container name and 404s.
	let mut wanted = HashSet::new();
	wanted.insert("proj-web-1".to_string());
	wanted.insert("proj-db-1".to_string());
	assert_eq!(
		containers_query(&wanted),
		"&containers=proj-db-1&containers=proj-web-1"
	);
}

#[test]
fn containers_query_empty_when_none_wanted() {
	assert_eq!(containers_query(&HashSet::new()), "");
}

fn name_set(names: &[&str]) -> HashSet<String> {
	names.iter().map(|s| s.to_string()).collect()
}

#[test]
fn stats_stream_break_is_fatal_only_while_a_sampled_container_still_runs() {
	let sampled = name_set(&["proj-web-1", "proj-db-1"]);

	// A sampled container is still running: the stream truncated a live sample,
	// so the error is a real failure (#1080) — the exit code a monitor needs.
	assert!(stats_stream_broke_mid_sample(
		&sampled,
		&name_set(&["proj-web-1"])
	));

	// Every sampled container has stopped: the stream ended because there was
	// nothing left to sample, and the missing terminal frame is the
	// finished-vs-broken ambiguity (#1104), not a fault.
	assert!(!stats_stream_broke_mid_sample(&sampled, &HashSet::new()));

	// A different, unrelated container is running (e.g. another project): it was
	// never sampled, so it does not make this stream's end a failure.
	assert!(!stats_stream_broke_mid_sample(
		&sampled,
		&name_set(&["other-app-1"])
	));
}

#[test]
fn stats_stream_break_with_no_sampled_containers_is_never_fatal() {
	// Nothing was being sampled, so no end can be a truncated-sample failure.
	assert!(!stats_stream_broke_mid_sample(
		&HashSet::new(),
		&name_set(&["proj-web-1"])
	));
}

#[test]
fn first_unknown_service_flags_typos() {
	let file =
		crate::parse_str("services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n")
			.unwrap();
	assert_eq!(first_unknown_service(&file, &[]), None);
	assert_eq!(
		first_unknown_service(&file, &["web".into(), "db".into()]),
		None
	);
	assert_eq!(
		first_unknown_service(&file, &["web".into(), "bogus".into()]),
		Some("bogus")
	);
}

/// The header and the rows are built from one set of widths, so every label
/// sits over its own column.
///
/// They were two hand-maintained layouts that nothing checked against each
/// other, and they had drifted: measured against a representative row, `MEM %`
/// sat one column past where its data ended and `PIDS` began exactly where its
/// data stopped, so the label was entirely off the column it named.
#[test]
fn the_header_lines_up_with_the_rows() {
	let row = format_row_with(&sample_stat(), false, false);
	let header = header();
	assert_eq!(
		header.chars().count(),
		row.chars().count(),
		"header and row must be the same width\nH: {header:?}\nR: {row:?}"
	);
	// Each label ends no later than its column does, which is what "over its own
	// column" means for a right-aligned numeric cell.
	for label in ["CPU %", "MEM %", "PIDS"] {
		let at = header.find(label).expect("label present");
		assert!(
			at + label.len() <= row.chars().count(),
			"{label} runs past the row\nH: {header:?}\nR: {row:?}"
		);
	}
}

/// A right-aligned label ends where its cell ends. `PIDS` used to start there
/// instead, which put the whole label past its data.
#[test]
fn the_pids_label_sits_over_the_pids_column() {
	let header = header();
	assert!(
		header.trim_end().ends_with("PIDS"),
		"PIDS is the trailing column: {header:?}"
	);
	let row = format_row_with(&sample_stat(), false, false);
	assert_eq!(
		header.rfind("PIDS").unwrap() + "PIDS".len(),
		row.chars().count(),
		"the label must end where the column does"
	);
}

/// Colour has to vary with the value or it says nothing. Everything under 70%
/// used to be the same green, so a container at 0.02% looked exactly like one
/// at 69%.
#[test]
fn the_load_bands_separate_idle_from_busy() {
	let idle = load_style(0.02);
	let moderate = load_style(40.0);
	let warm = load_style(60.0);
	let hot = load_style(95.0);
	assert_ne!(idle, moderate, "0.02% must not look like 40%");
	assert_ne!(moderate, warm, "40% must not look like 60%");
	assert_ne!(warm, hot, "60% must not look like 95%");
}

/// `--format json` never repaints, whatever the terminal is. Asserted on the
/// pure predicate because no piped test can reach it: with stdout not a
/// terminal the other half of the condition is already false, so a version that
/// let JSON repaint on a tty passed the whole suite.
#[test]
fn the_machine_path_never_repaints() {
	assert!(wants_region(false, true), "a table on a terminal repaints");
	assert!(!wants_region(true, true), "json on a terminal must not");
	assert!(!wants_region(false, false), "nor a table off a terminal");
	assert!(!wants_region(true, false));
}
