use super::{
	filter_orphans, is_valid_log_time, log_query, stream_broke_mid_output, validate_log_filters,
	LogsOptions,
};
use std::collections::HashSet;

#[cfg(unix)]
use crate::compose::types::{ComposeFile, Service};
#[cfg(unix)]
use crate::engine::fake_podman;
#[cfg(unix)]
use crate::engine::Engine;
#[cfg(unix)]
use crate::error::ComposeError;

#[cfg(unix)]
fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

/// #598: `--remove-orphans` that can't remove every orphan (e.g. an active
/// exec session) must not exit 0 with one silently left behind — but a
/// sibling orphan that removes cleanly must still be reclaimed.
#[tokio::test]
#[cfg(unix)]
async fn remove_orphans_propagates_a_real_failure_after_completing_the_rest() {
	// "web" is still declared in the file (known); the two "ghost" containers
	// are not, so both are orphans.
	let containers = r#"[
		{"Names":["/proj-web-1"]},
		{"Names":["/proj-ghost-1"]},
		{"Names":["/proj-ghost-2"]}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if method == "DELETE" && target.contains("/proj-ghost-1?force=true") {
			(200, String::new())
		} else if method == "DELETE" && target.contains("/proj-ghost-2?force=true") {
			(500, r#"{"message":"device or resource busy"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("web".into(), Service::default());

	let err = e
		.remove_orphans(&file)
		.await
		.expect_err("a real orphan-removal failure must propagate");
	assert!(
		matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
		"got {err:?}"
	);

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("/proj-ghost-1?force=true")),
		"expected proj-ghost-1 to still be removed despite proj-ghost-2 failing: {seen:?}"
	);
}

/// An orphan that is already gone (404) stays an idempotent no-op.
#[tokio::test]
#[cfg(unix)]
async fn remove_orphans_tolerates_already_gone() {
	let containers = r#"[{"Names":["/proj-ghost-1"]}]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let file = ComposeFile::default();

	e.remove_orphans(&file)
		.await
		.expect("an already-gone orphan must still exit 0");
}

/// After a runtime `scale web=3`, Podman has three `proj-web-*` containers
/// while the compose file still declares a single (unscaled) replica.
/// `logs web` must target every live container, not just the one the
/// static replica count predicts.
#[tokio::test]
#[cfg(unix)]
async fn logs_targets_every_live_replica_after_scale() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-web-2"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-web-3"],"Labels":{"podup.service":"web"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if method == "GET" && target.contains("/logs") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("web".into(), Service::default());

	e.logs_with_options(&file, &["web".to_string()], LogsOptions::default())
		.await
		.expect("logs should succeed");

	let seen = fake.requests.lock().unwrap();
	for i in 1..=3 {
		assert!(
			seen.iter()
				.any(|r| r.contains(&format!("/proj-web-{i}/logs"))),
			"expected proj-web-{i} to be targeted after scale: {seen:?}"
		);
	}
}

/// Resolving replicas for one selected service must not abort `logs` before
/// a single line prints for the others: `logs` already documents that it
/// tolerates a missing/not-yet-created container this way (see the
/// per-container `get_stream` handling in `logs_with_display`).
///
/// #1445 collapsed the per-service `live_replica_names` resolution into a
/// single bulk fetch (`live_project_replicas_sorted`), so the per-service
/// error path this test used to drive no longer exists — there is one
/// container-list call for the whole project, not one per service, so a
/// per-service 500 is no longer reachable in this code. The companion
/// `logs_fails_when_no_service_resolves_at_all` covers the only failure
/// mode that survives the bulk refactor: the bulk GET errors out, no
/// target survives, and `logs` exits non-zero instead of printing nothing
/// and reporting success.

#[test]
fn filter_orphans_keeps_only_unknown_names() {
	let known: HashSet<String> = ["web-1".to_string(), "db".to_string()].into();
	let names = vec![
		"web-1".to_string(),
		"db".to_string(),
		"old-cache".to_string(),
	];
	assert_eq!(filter_orphans(names, &known), vec!["old-cache".to_string()]);
}

#[test]
fn filter_orphans_empty_when_all_known() {
	let known: HashSet<String> = ["web".to_string()].into();
	assert!(filter_orphans(vec!["web".to_string()], &known).is_empty());
}

#[test]
fn log_query_defaults_to_stdout_stderr_no_follow() {
	let q = log_query(&LogsOptions::default());
	assert_eq!(q, "stdout=true&stderr=true&follow=false&timestamps=false");
}

#[test]
fn log_query_includes_set_options() {
	let q = log_query(&LogsOptions {
		follow: true,
		tail: Some("20".into()),
		since: Some("10m".into()),
		until: Some("2024-01-01T00:00:00".into()),
		timestamps: true,
	});
	assert!(q.contains("follow=true"));
	assert!(q.contains("timestamps=true"));
	assert!(q.contains("&tail=20"));
	assert!(q.contains("&since=10m"));
	// `:` is percent-encoded in the query value.
	assert!(q.contains("&until=2024-01-01T00%3A00%3A00"));
}

#[test]
fn validate_log_filters_accepts_good_values() {
	assert!(validate_log_filters(&LogsOptions {
		tail: Some("all".into()),
		since: Some("10m".into()),
		until: Some("2024-01-01T00:00:00Z".into()),
		..Default::default()
	})
	.is_ok());
	assert!(validate_log_filters(&LogsOptions {
		tail: Some("100".into()),
		since: Some("1700000000".into()),
		..Default::default()
	})
	.is_ok());
	assert!(validate_log_filters(&LogsOptions::default()).is_ok());
}

#[test]
fn validate_log_filters_rejects_bad_tail_and_time() {
	assert!(validate_log_filters(&LogsOptions {
		tail: Some("abc".into()),
		..Default::default()
	})
	.is_err());
	assert!(validate_log_filters(&LogsOptions {
		since: Some("yesterday".into()),
		..Default::default()
	})
	.is_err());
	assert!(validate_log_filters(&LogsOptions {
		until: Some("not-a-time".into()),
		..Default::default()
	})
	.is_err());
}

#[test]
fn is_valid_log_time_classifies_forms() {
	assert!(is_valid_log_time("10m"));
	assert!(is_valid_log_time("1h30m"));
	assert!(is_valid_log_time("500ms"));
	assert!(is_valid_log_time("1700000000"));
	assert!(is_valid_log_time("2024-01-02T03:04:05Z"));
	assert!(!is_valid_log_time("abc"));
	assert!(!is_valid_log_time(""));
	assert!(!is_valid_log_time("10x"));
}

/// The other side of the tolerance: when *nothing* resolves, there is no
/// partial result left to protect.
///
/// An unreachable engine made `logs` print nothing and exit 0, which is
/// indistinguishable from a project that simply has no logs — so a health check
/// or deploy gate built on `compose logs` read success from an engine that was
/// not there. Every service 500s here, so no target survives.
///
/// This is not #1104: nothing classifies how a stream *ended*. The requests
/// never opened.
#[tokio::test]
#[cfg(unix)]
async fn logs_fails_when_no_service_resolves_at_all() {
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(500, r#"{"message":"internal server error"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("a".into(), Service::default());
	file.services.insert("b".into(), Service::default());

	let out = e
		.logs_with_options(
			&file,
			&["a".to_string(), "b".to_string()],
			LogsOptions::default(),
		)
		.await;
	assert!(
		out.is_err(),
		"logs must not report success when it could reach nothing at all"
	);
}

/// And one container failing to stream while another succeeds stays tolerated,
/// so the logs that exist are still shown.
#[tokio::test]
#[cfg(unix)]
async fn logs_tolerates_one_container_that_will_not_stream() {
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			if target.contains("podup.service%3Da") {
				(200, r#"[{"Names":["/proj-a-1"]}]"#.to_string())
			} else {
				(200, r#"[{"Names":["/proj-b-1"]}]"#.to_string())
			}
		} else if method == "GET" && target.contains("/proj-a-1/logs") {
			(500, r#"{"message":"boom"}"#.to_string())
		} else if method == "GET" && target.contains("/logs") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("a".into(), Service::default());
	file.services.insert("b".into(), Service::default());

	e.logs_with_options(
		&file,
		&["a".to_string(), "b".to_string()],
		LogsOptions::default(),
	)
	.await
	.expect("one container failing to stream must not blank the other");
}

// ---------------------------------------------------------------------------
// #1104: telling a finished log stream from a broken one
// ---------------------------------------------------------------------------

#[test]
fn a_stream_ending_while_the_container_runs_is_a_break() {
	// The container outlived its own log stream, so output was truncated.
	assert!(stream_broke_mid_output(Some(true)));
}

#[test]
fn a_stream_ending_after_the_container_stopped_is_clean() {
	// The stream is supposed to end here; libpod just did not get to say so.
	assert!(!stream_broke_mid_output(Some(false)));
}

#[test]
fn an_unreadable_state_counts_as_a_break() {
	// The rule that is easy to get backwards, and the one that matters most: the
	// severed connection that ends the stream is usually the same one the
	// re-check needs, so reading "could not tell" as a clean end turns every
	// transport failure back into exit 0. Measured — the permissive version
	// reported a still-running container as stopped when the socket restarted
	// under an attached `logs -f`.
	assert!(stream_broke_mid_output(None));
}
