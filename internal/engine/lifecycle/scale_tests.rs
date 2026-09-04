//! Tests for the scale module: surplus-replica reconciliation, the
//! `running_replica_names` helper, the replica-limit / port-conflict /
//! fixed-name guard helpers, the bulk `live_project_replicas` listing that
//! powers the per-service lifecycle commands (#1363), and the sorted
//! `live_project_replicas_sorted` variant the per-replica query paths
//! (`exec`/`cp`/`port`/`logs`) share so a single container-list round-trip
//! powers them all (#1445).
//!
//! Same fixture pattern as the rest of the lifecycle suite: a unix-socket
//! fake libpod so the request shape (`all=true`, no `status=` filter) and
//! the per-service slice can both be pinned against a known server.

use super::scale::{
	check_fixed_name_scale, check_replica_limit, check_scale_port_conflict, state_is_active,
	DEFAULT_MAX_REPLICAS,
};

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

/// #598: a `scale`/`up --scale` down-sizing that can't remove a surplus
/// replica (e.g. an active exec session) must not exit 0 with it left
/// running — but a sibling replica that removes cleanly must still be
/// reclaimed.
#[tokio::test]
#[cfg(unix)]
async fn remove_surplus_replicas_propagates_a_real_rm_failure_after_completing_the_rest() {
	let live = r#"[{"Names":["/proj-web-1"]},{"Names":["/proj-web-2"]}]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, live.to_string())
		} else if (method == "POST" && target.contains("/stop"))
			|| (method == "DELETE" && target.contains("/proj-web-1?force=true"))
		{
			(200, String::new())
		} else if method == "DELETE" && target.contains("/proj-web-2?force=true") {
			(500, r#"{"message":"device or resource busy"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	// target = 0 desired replicas, so every live container is surplus
	// (mirrors the `replica_names_for_zero_scale_is_empty` contract).
	let err = e
		.remove_surplus_replicas("web", &crate::compose::types::Service::default(), 0)
		.await
		.expect_err("a real surplus-removal failure must propagate");
	assert!(
		matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
		"got {err:?}"
	);

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("/proj-web-1?force=true")),
		"expected proj-web-1 to still be removed despite proj-web-2 failing: {seen:?}"
	);
}

/// Surplus replicas that are already gone (404 on removal) stay an
/// idempotent no-op — a re-run of `scale` down must still exit 0.
#[tokio::test]
#[cfg(unix)]
async fn remove_surplus_replicas_tolerates_already_gone() {
	let live = r#"[{"Names":["/proj-web-1"]}]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, live.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	e.remove_surplus_replicas("web", &crate::compose::types::Service::default(), 0)
		.await
		.expect("an already-gone surplus replica must still exit 0");
}

/// libpod's `/containers/json` does not guarantee order; `logs` and every
/// other by-service lifecycle/query command must still see a scaled
/// service's replicas in the same ascending `-1, -2, -3` order the static
/// `replica_names_for` path always produces, even when the fake (like a
/// real libpod) hands them back shuffled. The bulk sorted helper
/// ([`Engine::live_project_replicas_sorted`]) is what now powers the
/// per-replica query paths (#1445); its per-bucket sort replaces the
/// per-service sort the old `live_replica_names` helper did.
#[tokio::test]
#[cfg(unix)]
async fn live_project_replicas_sorted_sorts_shuffled_replicas_ascending() {
	let containers = r#"[
		{"Names":["/proj-web-3"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-web-2"],"Labels":{"podup.service":"web"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let by_service = e
		.live_project_replicas_sorted()
		.await
		.expect("live_project_replicas_sorted should succeed");

	assert_eq!(
		by_service.get("web"),
		Some(&vec![
			"proj-web-1".to_string(),
			"proj-web-2".to_string(),
			"proj-web-3".to_string(),
		]),
		"the bucket must come back in ascending -1, -2, -3 order regardless of \
		 libpod's hand-back order"
	);
}

/// #1445: the bulk GET that the per-replica query paths now share must
/// cover a scaled service's full replica set, including replicas that are
/// stopped — `logs`/`port`/`exec` need to see the second replica even when
/// it is in `exited`, so the bulk request must NOT silently drop them the
/// way libpod's `runningOnly` default would.
#[tokio::test]
#[cfg(unix)]
async fn live_project_replicas_sorted_includes_stopped_replicas() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"State":"running","Labels":{"podup.service":"web"}},
		{"Names":["/proj-web-2"],"State":"running","Labels":{"podup.service":"web"}},
		{"Names":["/proj-web-3"],"State":"exited","Labels":{"podup.service":"web"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let by_service = e
		.live_project_replicas_sorted()
		.await
		.expect("live_project_replicas_sorted should succeed");

	// All three replicas — running and stopped — must be in the bucket.
	// A bug that silently filtered to running (libpod's `runningOnly`
	// default) would return just `proj-web-1` and `proj-web-2`, missing the
	// third replica entirely.
	assert_eq!(
		by_service.get("web"),
		Some(&vec![
			"proj-web-1".to_string(),
			"proj-web-2".to_string(),
			"proj-web-3".to_string(),
		]),
		"the stopped replica must not be filtered out — got {:?}",
		by_service.get("web")
	);
}

/// #1445: a service that exists in the compose file but has no live
/// container must NOT appear in the bulk map. The bulk helper does not see
/// the compose file; the per-replica query paths (`exec`/`cp`/`port`/
/// `logs`) layer the static-name fallback on top. Without that contract,
/// `logs` could not `docker compose logs`-style behave on a never-created
/// service — `port`/`exec`/`cp` would 404 immediately instead of letting
/// the caller see the predictable static name.
#[tokio::test]
#[cfg(unix)]
async fn live_project_replicas_sorted_omits_services_with_no_live_container() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			// Only `web` has any containers; `db` and `worker` are not yet
			// created.
			(
				200,
				r#"[{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}}]"#.to_string(),
			)
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let by_service = e
		.live_project_replicas_sorted()
		.await
		.expect("live_project_replicas_sorted should succeed");

	assert_eq!(
		by_service.get("web"),
		Some(&vec!["proj-web-1".to_string()]),
		"a service with a live container must appear in the map"
	);
	assert!(
		!by_service.contains_key("db"),
		"a service with no live container must NOT appear (caller falls back): {by_service:?}"
	);
	assert!(
		!by_service.contains_key("worker"),
		"a service with no live container must NOT appear (caller falls back): {by_service:?}"
	);
}

/// #1445: the per-replica query paths used to fan out one container-list
/// round-trip per service — 40 services meant 40 GETs for a single
/// `podup logs`. The bulk helper is the shared single GET. With the fake
/// pinned to answer only `/containers/json`, a single resolution call must
/// still satisfy the per-service query helpers without issuing a follow-up
/// GET. A regression that re-introduced per-service calls would make
/// `fake.requests` grow by one row for each lookup, so the assertion
/// below is the discriminator: counting requests is the test that fails if
/// the bulk path is bypassed.
#[tokio::test]
#[cfg(unix)]
async fn logs_resolves_replicas_in_one_bulk_get_not_one_per_service() {
	// Three services; `db` is scaled to 3, `web` to 2, `worker` to 1, all
	// running — so the bulk GET is the only fetch needed for `logs` to
	// find 6 targets across 3 services.
	let body = r#"[
		{"Names":["/proj-db-1"],"Labels":{"podup.service":"db"}},
		{"Names":["/proj-db-2"],"Labels":{"podup.service":"db"}},
		{"Names":["/proj-db-3"],"Labels":{"podup.service":"db"}},
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-web-2"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-worker-1"],"Labels":{"podup.service":"worker"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, body.to_string())
		} else if method == "GET" && target.contains("/logs") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = crate::compose::types::ComposeFile::default();
	file.services
		.insert("db".into(), crate::compose::types::Service::default());
	file.services
		.insert("web".into(), crate::compose::types::Service::default());
	file.services
		.insert("worker".into(), crate::compose::types::Service::default());

	// Drive `logs` once across all 3 services — the resolution path is the
	// one under test, not the streaming.
	let _ = e
		.logs_with_options(&file, &[], crate::engine::query::LogsOptions::default())
		.await;

	let seen = fake.requests.lock().unwrap();
	let bulk_gets = seen
		.iter()
		.filter(|r| r.starts_with("GET") && r.contains("/containers/json"))
		.count();
	assert_eq!(
		bulk_gets, 1,
		"logs must issue exactly one bulk GET for the whole project, \
		 not one per service. seen = {seen:?}"
	);
}

/// #1250: `top` aborted on a project with a stopped service because it asked
/// every container that exists for its process list, and libpod answers a
/// non-running one with an HTTP 500. The exited replica must be dropped
/// before the call, and the survivors must keep the ascending order every
/// other by-service command produces — so this asserts both, on a listing
/// that is shuffled and mixed-state at once.
#[tokio::test]
#[cfg(unix)]
async fn running_replica_names_drops_non_running_and_keeps_ascending_order() {
	let containers = r#"[
		{"Names":["/proj-web-3"],"State":"running"},
		{"Names":["/proj-web-4"],"State":"created"},
		{"Names":["/proj-web-1"],"State":"running"},
		{"Names":["/proj-web-5"],"State":"paused"},
		{"Names":["/proj-web-2"],"State":"exited"}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let names = e
		.running_replica_names("web")
		.await
		.expect("running_replica_names should succeed");

	assert_eq!(
		names,
		vec!["proj-web-1".to_string(), "proj-web-3".to_string()],
		"only the running replicas, in ascending order"
	);
}

/// The sibling half of the rule above: a service that exists in the compose
/// file but was never created has nothing running, so `top` must render
/// nothing for it rather than fall back to a statically-derived name and
/// then have to swallow the 404 that name earns.
#[tokio::test]
#[cfg(unix)]
async fn running_replica_names_does_not_fall_back_to_static_names() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let names = e
		.running_replica_names("web")
		.await
		.expect("running_replica_names should succeed");

	assert!(
		names.is_empty(),
		"a never-created service yields no names, got {names:?}"
	);
}

/// #1363 — the bulk project listing must include every project's
/// containers, not just the running ones. With `all=true` libpod drops the
/// `runningOnly` default and a 100-service project with 5 stopped replicas
/// returns all 100 names; the gotcha caught during validation was a bare
/// `GET /containers/json` (no `all=true`, no `status=...`) silently
/// filtering the stopped ones out, which would have made `start`/`restart`
/// act on a strict subset of the project's containers and leave the rest
/// untouched. Asserts both the request shape (carries `all=true`, no
/// `status=` workaround) and the returned map (every service present,
/// 100 names total).
#[tokio::test]
#[cfg(unix)]
async fn live_project_replicas_returns_every_project_container_including_stopped() {
	// 100 services; 95 running + 5 stopped, one container each.
	let entries: Vec<String> = (0..100)
		.map(|i| {
			let svc = format!("svc{i:03}");
			let state = if i % 20 == 0 { "exited" } else { "running" };
			format!(
				r#"{{"Names":["/proj-{svc}-1"],"State":"{state}","Labels":{{"podup.service":"{svc}"}}}}"#
			)
		})
		.collect();
	let body = format!("[{}]", entries.join(","));
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, body.clone())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let by_service = e
		.live_project_replicas()
		.await
		.expect("live_project_replicas should succeed");

	assert_eq!(
		by_service.len(),
		100,
		"every project service must be in the map, got {} entries",
		by_service.len()
	);
	let total_names: usize = by_service.values().map(Vec::len).sum();
	assert_eq!(
		total_names, 100,
		"every project container must be returned (5 stopped included)"
	);
	// The 5 stopped services (svc000, svc020, …, svc080) must each be
	// present with their replica name — i.e. `all=true` actually carried
	// past the default filter, not just "runningOnly expanded to one".
	for i in (0..100).step_by(20) {
		let svc = format!("svc{i:03}");
		let names = by_service
			.get(&svc)
			.unwrap_or_else(|| panic!("missing {svc}"));
		assert_eq!(names, &vec![format!("proj-{svc}-1")], "{svc}");
	}

	// The request path must include `all=true`: libpod's container-list
	// defaults to `runningOnly` when no `status` filter is supplied, so
	// the bulk GET that powers the per-service lifecycle commands would
	// silently drop the very containers those commands need to act on.
	let seen = fake.requests.lock().unwrap();
	let container_list = seen
		.iter()
		.find(|r| r.contains("GET") && r.contains("/containers/json"))
		.expect("exactly one bulk GET to /containers/json");
	assert!(
		container_list.contains("all=true"),
		"the bulk GET must pass all=true, got {container_list:?}"
	);
	// And no `status=` filter sneaked in that would have achieved the
	// same effect — we want the all-inclusive default, not a workaround.
	assert!(
		!container_list.contains("status="),
		"the bulk GET must rely on all=true alone, not a status filter: {container_list:?}"
	);
}

#[test]
fn state_is_active_only_for_running_and_paused() {
	// `stop` actually transitions only a running or paused container; for any
	// other state it is a no-op that must not be reported as "stopped" (#876).
	assert!(state_is_active("running"));
	assert!(state_is_active("paused"));
	assert!(!state_is_active("created"));
	assert!(!state_is_active("exited"));
	assert!(!state_is_active("stopped"));
	assert!(!state_is_active("dead"));
	assert!(!state_is_active("configured"));
	assert!(!state_is_active(""));
}

#[test]
fn replica_limit_default_and_env_override() {
	// One test owns the shared `PODUP_MAX_REPLICAS` env var for its whole body
	// so a sibling test running in parallel can never race it.
	let max = DEFAULT_MAX_REPLICAS as usize;

	// Default ceiling: at-limit allowed, over-limit rejected.
	std::env::remove_var("PODUP_MAX_REPLICAS");
	assert!(check_replica_limit("web", 1).is_ok());
	assert!(check_replica_limit("web", max).is_ok());
	let err = check_replica_limit("web", max + 1).unwrap_err();
	assert!(matches!(
		err,
		crate::error::ComposeError::ReplicaLimitExceeded { .. }
	));
	assert!(check_replica_limit("web", 100_000).is_err());

	// Env override lowers the ceiling.
	std::env::set_var("PODUP_MAX_REPLICAS", "2");
	assert!(check_replica_limit("web", 2).is_ok());
	assert!(check_replica_limit("web", 3).is_err());

	// A zero/garbage override falls back to the default ceiling.
	std::env::set_var("PODUP_MAX_REPLICAS", "0");
	assert!(check_replica_limit("web", max).is_ok());
	std::env::set_var("PODUP_MAX_REPLICAS", "nope");
	assert!(check_replica_limit("web", max).is_ok());
	std::env::remove_var("PODUP_MAX_REPLICAS");
}

fn service(yaml: &str) -> crate::compose::types::Service {
	let file = crate::parse_str(yaml).unwrap();
	file.services.into_iter().next().unwrap().1
}

#[test]
fn single_replica_never_conflicts() {
	let svc = service("services:\n  web:\n    image: x\n    ports:\n      - \"8080:80\"\n");
	assert!(check_scale_port_conflict("web", &svc, 1).is_ok());
}

#[test]
fn scaled_fixed_host_port_conflicts() {
	let svc = service("services:\n  web:\n    image: x\n    ports:\n      - \"8080:80\"\n");
	let err = check_scale_port_conflict("web", &svc, 3).unwrap_err();
	assert!(matches!(
		err,
		crate::error::ComposeError::ScalePortConflict { .. }
	));
	assert!(err.to_string().contains("8080"));
}

#[test]
fn scaled_random_host_port_is_allowed() {
	// A container-only port (`"80"`) gets a runtime-assigned host port per
	// replica, so scaling is fine.
	let svc = service("services:\n  web:\n    image: x\n    ports:\n      - \"80\"\n");
	assert!(check_scale_port_conflict("web", &svc, 3).is_ok());
}

#[test]
fn scaled_no_ports_is_allowed() {
	let svc = service("services:\n  worker:\n    image: x\n");
	assert!(check_scale_port_conflict("worker", &svc, 5).is_ok());
}

#[test]
fn fixed_container_name_single_replica_is_allowed() {
	let svc = service("services:\n  app:\n    image: x\n    container_name: myapp\n");
	assert!(check_fixed_name_scale("app", &svc, 1).is_ok());
}

#[test]
fn fixed_container_name_scaled_above_one_is_rejected() {
	let svc = service("services:\n  app:\n    image: x\n    container_name: myapp\n");
	let err = check_fixed_name_scale("app", &svc, 3).unwrap_err();
	assert!(matches!(err, crate::error::ComposeError::Unsupported(_)));
	assert!(err.to_string().contains("container_name"));
}

#[test]
fn unnamed_service_scales_freely() {
	let svc = service("services:\n  app:\n    image: x\n");
	assert!(check_fixed_name_scale("app", &svc, 5).is_ok());
}

/// #1445 is a round-trip count, so pin the count rather than only the values it
/// produces. Before it, every selected service cost its own `/containers/json`
/// GET; a project of N services made N of them. Nothing in the value assertions
/// above notices if that regresses — the names come back identical either way —
/// so a later refactor could quietly put the call back inside the loop.
///
/// Four services, and the fake records every request it answers. The assertion
/// is that exactly one container-list GET reaches the socket.
#[tokio::test]
#[cfg(unix)]
async fn logs_issues_one_container_list_for_the_whole_project() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"State":"running","Labels":{"podup.service":"web"}},
		{"Names":["/proj-api-1"],"State":"running","Labels":{"podup.service":"api"}},
		{"Names":["/proj-db-1"],"State":"running","Labels":{"podup.service":"db"}},
		{"Names":["/proj-cache-1"],"State":"running","Labels":{"podup.service":"cache"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else {
			// Every per-container logs stream 404s: this test is about how many
			// listing calls are made, not about what the streams carry.
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let file = crate::parse_str(
		"services:\n  web:\n    image: x\n  api:\n    image: x\n  db:\n    image: x\n  cache:\n    image: x\n",
	)
	.unwrap();

	// The per-container streams all 404, so the call itself is expected to
	// fail; the request log is what carries the answer.
	let _ = e.logs(&file, None, false).await;

	let lists = fake
		.requests
		.lock()
		.unwrap()
		.iter()
		.filter(|r| r.contains("/containers/json"))
		.count();
	assert_eq!(
		lists,
		1,
		"four services must share one container-list round-trip, not one each (#1445); \
		 requests were {:?}",
		fake.requests.lock().unwrap()
	);
}

/// A surplus replica's row opens with `Stopping` before the stop request,
/// moves to `Removing` before the delete, and closes with `Removed`. The row
/// used to appear only at `Removed`, with no start time and nothing on screen
/// during a ten-second grace (#1686).
#[tokio::test]
#[cfg(unix)]
async fn stop_and_remove_opens_the_row_before_the_stop_and_closes_it_removed() {
	let live = r#"[{"Names":["/proj-web-1"]},{"Names":["/proj-web-2"]}]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, live.to_string())
		} else if (method == "POST" && target.contains("/stop"))
			|| (method == "DELETE" && target.contains("/proj-web-2?force=true"))
		{
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let capture = crate::ui::progress::capture::Capture::start();
	e.remove_surplus_replicas("web", &crate::compose::types::Service::default(), 1)
		.await
		.expect("the surplus replica is removed");
	let verbs: Vec<String> = capture
		.verbs()
		.into_iter()
		.filter(|(_, name, _)| name == "proj-web-2")
		.map(|(_, _, verb)| verb)
		.collect();
	assert_eq!(
		verbs,
		vec!["Stopping", "Removing", "Removed"],
		"the row is opened before the stop and closed after the delete"
	);
}
