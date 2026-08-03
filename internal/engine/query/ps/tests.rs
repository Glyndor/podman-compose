use super::*;
use std::collections::HashMap;

#[cfg(unix)]
use crate::engine::fake_podman;

#[cfg(unix)]
fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

fn entry(status: &str, state: &str) -> ContainerListEntry {
	ContainerListEntry {
		id: "abc123".into(),
		names: vec!["/web".into()],
		image: "alpine".into(),
		status: status.into(),
		state: state.into(),
		ports: vec![],
		exit_code: None,
		labels: HashMap::new(),
		// Absent by default: the fixtures below exercise the fallback path, and
		// the ones that render an age set these explicitly.
		started_at: 0,
		created: String::new(),
	}
}

/// A fixed `now` for the cells that render an age, so the tests are
/// deterministic. 2026-08-03 00:58:45 UTC, the instant captured from libpod.
const NOW: i64 = 1_785_718_725;

#[test]
fn status_matches_empty_filter_matches_all() {
	assert!(status_matches("running", &[]));
	assert!(status_matches("exited", &[]));
}

#[test]
fn status_matches_is_case_insensitive_prefix() {
	assert!(status_matches("running", &["RUNNING".to_string()]));
	assert!(status_matches("exited", &["exit".to_string()]));
	assert!(!status_matches("running", &["exited".to_string()]));
	// An empty wanted value never matches.
	assert!(!status_matches("running", &["".to_string()]));
}

#[test]
fn split_ps_filters_buckets_known_keys_and_flags_unknown() {
	let (status, names, unknown) = split_ps_filters(&[
		"status=running".to_string(),
		"name=web".to_string(),
		"label=foo".to_string(),
	]);
	assert_eq!(status, vec!["running".to_string()]);
	assert_eq!(names, vec!["web".to_string()]);
	assert_eq!(unknown, vec!["label=foo".to_string()]);
}

#[test]
fn display_status_falls_back_to_state_when_status_empty() {
	assert_eq!(display_status(&entry("", "running")), "running");
	assert_eq!(display_status(&entry("", "exited")), "exited");
}

#[test]
fn display_status_prefers_status_when_present() {
	assert_eq!(
		display_status(&entry("Up 2 seconds", "running")),
		"Up 2 seconds"
	);
}

fn entry_exit(state: &str, code: Option<i32>) -> ContainerListEntry {
	ContainerListEntry {
		exit_code: code,
		..entry("", state)
	}
}

#[test]
fn table_status_shows_exit_code_for_bare_exited() {
	// A crash (non-zero) and a clean exit (zero) must be distinguishable,
	// even though libpod reports both as a bare `exited` state.
	assert_eq!(
		table_status(&entry_exit("exited", Some(7)), NOW),
		"Exited (7)"
	);
	assert_eq!(
		table_status(&entry_exit("exited", Some(0)), NOW),
		"Exited (0)"
	);
	// Missing exit code defaults to 0 rather than rendering a bare word.
	assert_eq!(table_status(&entry_exit("exited", None), NOW), "Exited (0)");
	// `dead` is treated like an exit too.
	assert_eq!(
		table_status(&entry_exit("dead", Some(255)), NOW),
		"Exited (255)"
	);
}

#[test]
fn table_status_keeps_running_and_rich_status_text() {
	assert_eq!(
		table_status(&entry("Up 2 seconds", "running"), NOW),
		"Up 2 seconds"
	);
	assert_eq!(table_status(&entry("", "running"), NOW), "running");
	// A Docker-style status that already carries the code is left untouched.
	let c = ContainerListEntry {
		exit_code: Some(7),
		..entry("Exited (7) 4 seconds ago", "exited")
	};
	assert_eq!(table_status(&c, NOW), "Exited (7) 4 seconds ago");
}

#[test]
fn format_ports_defaults_missing_host_ip_to_all_interfaces() {
	let p = ContainerPort {
		host_ip: None,
		host_port: Some(8080),
		container_port: 80,
		protocol: Some("tcp".into()),
		..Default::default()
	};
	assert_eq!(
		format_ports(std::slice::from_ref(&p)),
		"0.0.0.0:8080->80/tcp"
	);
}

#[test]
fn format_ports_keeps_explicit_host_ip() {
	let p = ContainerPort {
		host_ip: Some("127.0.0.1".into()),
		host_port: Some(5432),
		container_port: 5432,
		..Default::default()
	};
	assert_eq!(
		format_ports(std::slice::from_ref(&p)),
		"127.0.0.1:5432->5432"
	);
}

#[test]
fn format_ports_expands_a_collapsed_range() {
	// libpod collapses 51251-51253->8080-8082 into one record with range=3;
	// the full range must be rendered, not just the first mapping.
	let p = ContainerPort {
		host_ip: None,
		host_port: Some(51251),
		container_port: 8080,
		protocol: Some("tcp".into()),
		range: Some(3),
	};
	assert_eq!(
		format_ports(std::slice::from_ref(&p)),
		"0.0.0.0:51251-51253->8080-8082/tcp"
	);
}

#[test]
fn format_port_record_does_not_overflow_u16_on_pathological_range() {
	// `host_port`/`container_port`/`range` come straight from libpod's JSON —
	// untrusted input. host_port=65535 with a range of 2 needs
	// `host_port + (range - 1)` = 65536, which does not fit in a u16: it
	// wraps to 0 in release and panics under overflow-checks (this test runs
	// in a debug build, so a regression here panics rather than silently
	// passing). The rendered end-of-range must show the real, wider number
	// instead of wrapping.
	let p = ContainerPort {
		host_ip: None,
		host_port: Some(65535),
		container_port: 65535,
		protocol: Some("tcp".into()),
		range: Some(2),
	};
	let rendered = format_ports(std::slice::from_ref(&p));
	assert_eq!(rendered, "0.0.0.0:65535-65536->65535-65536/tcp");
}

#[test]
fn publishers_expand_each_port_in_a_range() {
	let p = ContainerPort {
		host_ip: Some("0.0.0.0".into()),
		host_port: Some(51251),
		container_port: 8080,
		protocol: Some("tcp".into()),
		range: Some(3),
	};
	let pubs = publishers(std::slice::from_ref(&p));
	assert_eq!(pubs.len(), 3);
	assert_eq!(pubs[0]["TargetPort"], 8080);
	assert_eq!(pubs[0]["PublishedPort"], 51251);
	assert_eq!(pubs[2]["TargetPort"], 8082);
	assert_eq!(pubs[2]["PublishedPort"], 51253);
	assert_eq!(pubs[1]["Protocol"], "tcp");
}

#[test]
fn publishers_does_not_overflow_u16_on_pathological_range() {
	// Same untrusted-input hazard as `format_port_record`: expanding
	// container_port=65535 over a range of 2 must not wrap the last entry's
	// TargetPort/PublishedPort to 0.
	let p = ContainerPort {
		host_ip: Some("0.0.0.0".into()),
		host_port: Some(65535),
		container_port: 65535,
		protocol: Some("tcp".into()),
		range: Some(2),
	};
	let pubs = publishers(std::slice::from_ref(&p));
	assert_eq!(pubs.len(), 2);
	assert_eq!(pubs[1]["TargetPort"], 65536);
	assert_eq!(pubs[1]["PublishedPort"], 65536);
}

#[test]
fn health_is_derived_from_status_text() {
	assert_eq!(health_from_status("Up 2 minutes (healthy)"), "healthy");
	assert_eq!(health_from_status("Up 1 minute (unhealthy)"), "unhealthy");
	assert_eq!(
		health_from_status("Up 3 seconds (health: starting)"),
		"starting"
	);
	assert_eq!(health_from_status("Exited (1) 4 seconds ago"), "");
	// A restarting container with no healthcheck must not be misread as
	// "starting" health — only the real `health: starting` token counts.
	assert_eq!(health_from_status("Restarting (1) 3 seconds ago"), "");
}

/// A running container says how long it has been up, which is what both
/// reference tools do and what podup did not. Measured the same day: `podman ps`
/// renders `Up 13 hours (healthy)` and `docker compose ps` renders
/// `Up 2 minutes`, while podup rendered the bare word `running`.
#[test]
fn a_running_container_reports_how_long_it_has_been_up() {
	let c = ContainerListEntry {
		started_at: NOW - (2 * 3600 + 5 * 60 + 3),
		..entry("", "running")
	};
	assert_eq!(table_status(&c, NOW), "Up 2h 5m 3s");
}

/// The health suffix appears only when the container has a healthcheck. libpod
/// leaves `Status` empty otherwise, so there is nothing to append rather than an
/// unknown to invent.
#[test]
fn the_health_suffix_appears_only_when_there_is_a_healthcheck() {
	let started = NOW - 13 * 3600;
	let healthy = ContainerListEntry {
		started_at: started,
		..entry("healthy", "running")
	};
	let unhealthy = ContainerListEntry {
		started_at: started,
		..entry("unhealthy", "running")
	};
	let plain = ContainerListEntry {
		started_at: started,
		..entry("", "running")
	};
	assert_eq!(table_status(&healthy, NOW), "Up 13h (healthy)");
	assert_eq!(table_status(&unhealthy, NOW), "Up 13h (unhealthy)");
	assert_eq!(table_status(&plain, NOW), "Up 13h");
}

/// A zero `StartedAt` means the field was absent, not that the container
/// started at the epoch. Rendering `Up 56y` for a container that has never run
/// is worse than saying nothing, so the cell falls back to the state.
#[test]
fn an_absent_start_time_does_not_become_an_age() {
	let c = ContainerListEntry {
		started_at: 0,
		..entry("", "running")
	};
	assert_eq!(table_status(&c, NOW), "running");
}

/// A start time in the future is clock skew between this process and the
/// server, not a negative age. `Up 0s` on a container that just started is
/// right; `Up -3s` is never right.
#[test]
fn a_start_time_in_the_future_clamps_to_zero() {
	let c = ContainerListEntry {
		started_at: NOW + 3,
		..entry("", "running")
	};
	assert_eq!(table_status(&c, NOW), "Up 0s");
}

/// Only a running container gets an age. A stopped one keeps the exit code it
/// already reported, which is the more useful fact about it.
#[test]
fn only_a_running_container_reports_an_age() {
	let c = ContainerListEntry {
		started_at: NOW - 3600,
		..entry_exit("exited", Some(7))
	};
	assert_eq!(table_status(&c, NOW), "Exited (7)");
}

/// CREATED is how long ago the container was made, parsed from the RFC 3339
/// string libpod sends in `Created`.
#[test]
fn the_created_cell_reports_the_age_of_the_container() {
	let c = ContainerListEntry {
		// 2026-08-02 22:34:41 at -05:00 is 2026-08-03 03:34:41Z, which is two
		// hours and change after NOW... so use a value comfortably before it.
		created: "2026-08-01T00:58:45Z".into(),
		..entry("", "running")
	};
	assert_eq!(table_created(&c, NOW), "2d");
}

/// A timestamp podup cannot parse leaves the cell blank rather than showing a
/// plausible wrong age. The wrong age is the one a reader would act on.
#[test]
fn an_unparseable_created_leaves_the_cell_blank() {
	for bad in ["", "not a timestamp", "2026-13-01T00:00:00Z"] {
		let c = ContainerListEntry {
			created: bad.into(),
			..entry("", "running")
		};
		assert_eq!(table_created(&c, NOW), "", "{bad:?}");
	}
}

/// The JSON path passes the wire values through rather than a rendering. A
/// machine consumer wants an instant it can compute with, and `docker compose ps
/// --format json` passes the RFC 3339 string through too.
#[test]
fn the_json_row_carries_the_raw_instants() {
	let c = ContainerListEntry {
		started_at: 1_785_728_082,
		created: "2026-08-02T22:34:41.982670-05:00".into(),
		..entry("", "running")
	};
	let row = ps_json_row(&c);
	assert_eq!(row["StartedAt"], serde_json::json!(1_785_728_082_i64));
	assert_eq!(
		row["Created"],
		serde_json::json!("2026-08-02T22:34:41.982670-05:00")
	);
}

#[test]
fn ps_json_row_surfaces_state_exitcode_and_publishers() {
	let mut labels = HashMap::new();
	labels.insert("podup.project".to_string(), "demo".to_string());
	labels.insert("podup.service".to_string(), "web".to_string());
	let c = ContainerListEntry {
		id: "deadbeef".into(),
		names: vec!["/demo-web-1".into()],
		image: "nginx:1.25".into(),
		status: "Exited (137) 2s ago".into(),
		state: "exited".into(),
		ports: vec![ContainerPort {
			host_ip: None,
			host_port: Some(8080),
			container_port: 80,
			protocol: Some("tcp".into()),
			range: None,
		}],
		exit_code: Some(137),
		labels,
		started_at: 1_785_728_082,
		created: "2026-08-02T22:34:41.982670-05:00".into(),
	};
	let row = ps_json_row(&c);
	assert_eq!(row["Name"], "demo-web-1");
	assert_eq!(row["Service"], "web");
	assert_eq!(row["Project"], "demo");
	assert_eq!(row["State"], "exited");
	assert_eq!(row["ExitCode"], 137);
	assert_eq!(row["ID"], "deadbeef");
	assert_eq!(row["Publishers"][0]["PublishedPort"], 8080);
}

/// `ps --status exited` (or `--filter status=exited`) without `-a` must
/// still find exited containers: libpod's list endpoint only returns
/// running containers when `all=false`, so a status filter must force
/// `all=true` on the outgoing request regardless of `opts.all`.
#[tokio::test]
#[cfg(unix)]
async fn ps_status_filter_forces_all_true_even_when_opts_all_is_false() {
	let fake = fake_podman::start(|method, _target| {
		if method == "GET" {
			(200, "[]".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let file = ComposeFile::default();

	e.ps_filtered(
		&file,
		PsOptions {
			all: false,
			..Default::default()
		},
		PsFilterOptions {
			status: vec!["exited".into()],
			..Default::default()
		},
	)
	.await
	.expect("ps should succeed");

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("/containers/json") && r.contains("all=true")),
		"a status filter must force all=true even with opts.all=false: {seen:?}"
	);
}

/// Without a status filter, `opts.all=false` stays `all=false` on the wire
/// (the common case: `ps` with no flags lists only running containers).
#[tokio::test]
#[cfg(unix)]
async fn ps_without_status_filter_keeps_all_false_by_default() {
	let fake = fake_podman::start(|method, _target| {
		if method == "GET" {
			(200, "[]".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let file = ComposeFile::default();

	e.ps_filtered(&file, PsOptions::default(), PsFilterOptions::default())
		.await
		.expect("ps should succeed");

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("/containers/json") && r.contains("all=false")),
		"no status filter must keep all=false: {seen:?}"
	);
}
