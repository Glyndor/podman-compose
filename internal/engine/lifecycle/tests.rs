use super::container_rm_path;

#[cfg(unix)]
use super::Engine;
#[cfg(unix)]
use crate::compose::types::{ComposeFile, Service};
#[cfg(unix)]
use crate::engine::fake_podman;
#[cfg(unix)]
use crate::error::ComposeError;

#[cfg(unix)]
fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

/// #598: a repeated `up` finding a stopped-but-unchanged container must not
/// silently succeed when the start genuinely fails (e.g. its published host
/// port is now taken by something else).
#[tokio::test]
#[cfg(unix)]
async fn ensure_started_propagates_a_real_start_failure() {
	let fake = fake_podman::start(|method, target| {
		if method == "POST" && target.contains("/proj-web-1/start") {
			(500, r#"{"message":"address already in use"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");
	let err = e
		.ensure_started("proj-web-1")
		.await
		.expect_err("a real start failure must propagate, not exit 0 silently");
	match err {
		ComposeError::Podman(pe) => assert!(pe.is_status(500), "got {pe}"),
		other => panic!("expected a Podman error, got {other:?}"),
	}
	assert!(
		fake.requests
			.lock()
			.unwrap()
			.iter()
			.any(|r| r.contains("/proj-web-1/start")),
		"expected the fake socket to have received the start request"
	);
}

/// A container that vanished between the presence check and the start
/// (or one Podman reports as already running, 304) is an idempotent no-op,
/// matching `run_lifecycle_op`.
#[tokio::test]
#[cfg(unix)]
async fn ensure_started_tolerates_404_and_304() {
	let fake = fake_podman::start(|_, _| (404, r#"{"message":"no such container"}"#.to_string()));
	let e = engine_with(fake.client(), "proj");
	e.ensure_started("proj-web-1")
		.await
		.expect("404 must be an idempotent no-op");

	let fake = fake_podman::start(|_, _| (304, String::new()));
	let e = engine_with(fake.client(), "proj");
	e.ensure_started("proj-web-1")
		.await
		.expect("304 must be an idempotent no-op");
}

/// Two containers to tear down: one whose removal genuinely fails (a busy
/// mount, an active exec session), one that removes cleanly. `down` must
/// still attempt (and complete) the second before exiting non-zero for the
/// first (#598) — a CI teardown must not be told it succeeded.
#[tokio::test]
#[cfg(unix)]
async fn down_propagates_a_real_removal_failure_after_completing_the_rest() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-db-1"],"Labels":{"podup.service":"db"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if method == "POST" && target.contains("/stop") {
			(200, String::new())
		} else if method == "DELETE" && target.contains("/proj-web-1?force=true") {
			(500, r#"{"message":"device or resource busy"}"#.to_string())
		} else if method == "DELETE" && target.contains("/proj-db-1?force=true") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("web".into(), Service::default());
	file.services.insert("db".into(), Service::default());

	let err = e
		.down_with_options(&file, false)
		.await
		.expect_err("a real container-removal failure must propagate");
	assert!(
		matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
		"got {err:?}"
	);

	// Best-effort: the healthy container must still have been reached even
	// though the other one failed.
	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("/proj-db-1?force=true")),
		"expected proj-db-1 to be removed despite proj-web-1 failing: {seen:?}"
	);
}

/// #598 regression: `container_rm_path` always forces removal (`?force=true`),
/// which SIGKILLs the container regardless of how `stop` went — so a stop
/// that fails or stalls (HTTP 500) is superseded, not fatal, once the
/// force-remove that follows it succeeds. This pins the exact gap that let the
/// bug through: folding the `stop` failure into `first_err` made `down` return
/// `Err` even though teardown fully succeeded.
#[tokio::test]
#[cfg(unix)]
async fn down_tolerates_a_stalled_stop_when_the_force_remove_succeeds() {
	let containers = r#"[{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}}]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if method == "POST" && target.contains("/stop") {
			(
				500,
				r#"{"message":"timed out waiting for container to exit"}"#.to_string(),
			)
		} else if method == "DELETE" && target.contains("/proj-web-1?force=true") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("web".into(), Service::default());

	e.down_with_options(&file, false)
		.await
		.expect("a stalled/failed stop superseded by a successful force-remove must not fail down");

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("POST") && r.contains("/stop")),
		"expected the stop to have been attempted: {seen:?}"
	);
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("/proj-web-1?force=true")),
		"expected the force-remove to have been attempted: {seen:?}"
	);
}

/// A second `down` on an already torn-down project (no live containers,
/// nothing left to sweep) must still exit 0 — idempotency is preserved.
#[tokio::test]
#[cfg(unix)]
async fn down_on_an_already_torn_down_project_is_still_ok() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("web".into(), Service::default());

	e.down_with_options(&file, false)
		.await
		.expect("a re-run down on a torn-down project must still exit 0");
}

/// `down` now walks dependency levels (reversed) instead of a flat reversed
/// order, fanning out within a level via `join_bounded`. `web depends_on db`
/// puts web alone in the first (post-reversal) level and db alone in the
/// second, so this isolates the cross-level ordering guarantee from
/// within-level concurrency: web's whole teardown (stop + rm) must complete
/// before db is even asked to stop, exactly like the pre-parallel flat
/// reversed-order walk.
#[tokio::test]
#[cfg(unix)]
async fn down_tears_down_dependent_levels_before_their_dependencies() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-db-1"],"Labels":{"podup.service":"db"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if (method == "POST" && target.contains("/stop"))
			|| (method == "DELETE" && target.contains("force=true"))
		{
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str(
		"services:\n  db:\n    image: x\n  web:\n    image: x\n    depends_on:\n      - db\n",
	)
	.unwrap();

	e.down_with_options(&file, false)
		.await
		.expect("a healthy two-level teardown must succeed");

	let seen = fake.requests.lock().unwrap();
	let web_rm = seen
		.iter()
		.position(|r| r.contains("DELETE") && r.contains("proj-web-1?force=true"))
		.expect("web must have been removed");
	let db_stop = seen
		.iter()
		.position(|r| r.contains("POST") && r.contains("proj-db-1") && r.contains("stop"))
		.expect("db must have been stopped");
	assert!(
		web_rm < db_stop,
		"expected web's level to fully complete before db's level's stop begins: {seen:?}"
	);
}

/// `web` and `cache` share no `depends_on` relationship, so `resolve_levels`
/// groups them into a single level and `down_with_options` dispatches both
/// containers' teardown through the same `join_bounded` (`buffer_unordered`)
/// mechanism proven order-independent by
/// `parallel::tests::join_bounded_preserves_input_order`, instead of one
/// await-per-container in strict sequence. Asserting real wall-clock overlap
/// against a synchronous test responder would require a multi-thread runtime
/// and a blocking rendezvous inside the fake — a source of exactly the
/// flakiness the testing standard forbids — so this test instead pins the
/// dispatch contract (both containers are targeted, the level completes) and
/// leaves the concurrency guarantee itself to `join_bounded`'s own test plus
/// the code structure (no `.await` between the two containers' futures).
#[tokio::test]
#[cfg(unix)]
async fn down_targets_every_independent_service_within_one_level() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-cache-1"],"Labels":{"podup.service":"cache"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if (method == "POST" && target.contains("/stop"))
			|| (method == "DELETE" && target.contains("force=true"))
		{
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert("web".into(), Service::default());
	file.services.insert("cache".into(), Service::default());

	e.down_with_options(&file, false)
		.await
		.expect("a healthy single-level teardown of two independent services must succeed");

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("proj-web-1?force=true")),
		"expected web to have been targeted: {seen:?}"
	);
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("proj-cache-1?force=true")),
		"expected cache to have been targeted: {seen:?}"
	);
}

/// Determinism regression: with `web depends_on db`, both containers' removal
/// fail with distinct statuses. Levels are visited in a fixed order (web's
/// level first, post-reversal), so `down` must return web's failure — never
/// db's — regardless of how `join_bounded`'s internal `buffer_unordered`
/// happens to interleave completions within each level. db's level must still
/// be attempted (best-effort teardown continues past the first failing
/// level).
#[tokio::test]
#[cfg(unix)]
async fn down_first_error_is_deterministic_across_levels() {
	let containers = r#"[
		{"Names":["/proj-web-1"],"Labels":{"podup.service":"web"}},
		{"Names":["/proj-db-1"],"Labels":{"podup.service":"db"}}
	]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if method == "POST" && target.contains("/stop") {
			(200, String::new())
		} else if method == "DELETE" && target.contains("proj-web-1?force=true") {
			(500, r#"{"message":"web busy"}"#.to_string())
		} else if method == "DELETE" && target.contains("proj-db-1?force=true") {
			(503, r#"{"message":"db unavailable"}"#.to_string())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str(
		"services:\n  db:\n    image: x\n  web:\n    image: x\n    depends_on:\n      - db\n",
	)
	.unwrap();

	let err = e
		.down_with_options(&file, false)
		.await
		.expect_err("both levels fail to remove their container");
	assert!(
		matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
		"expected web's (first, post-reversal) level failure, not db's: {err:?}"
	);

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter()
			.any(|r| r.contains("DELETE") && r.contains("proj-db-1?force=true")),
		"expected db's level to still be attempted after web's level failed: {seen:?}"
	);
}

/// #6.3 regression: before this fix, each service's image was pulled inside
/// `up_one_service`, gated behind the level barrier — so a level-2 service's
/// pull never even started until level 1 finished. `web depends_on db` puts
/// db alone in level 1 and web alone in level 2; both images must now be
/// pulled by the up-front prefetch stage, before the very first container is
/// created, instead of web's pull waiting its turn behind db's whole level.
#[tokio::test]
#[cfg(unix)]
async fn up_prefetches_every_levels_image_before_any_container_create() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "POST" && target.contains("/containers/create") {
			(200, "{}".to_string())
		} else if method == "POST" && target.contains("/start") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str(
		"services:\n  db:\n    image: img-db\n  web:\n    image: img-web\n    depends_on:\n      - db\n",
	)
	.unwrap();

	e.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect("a healthy two-level up must succeed");

	let seen = fake.requests.lock().unwrap();
	let first_create = seen
		.iter()
		.position(|r| r.contains("/containers/create"))
		.expect("expected at least one container create");
	let db_pull = seen
		.iter()
		.position(|r| r.contains("/images/pull") && r.contains("img-db"))
		.expect("expected db's image to be pulled");
	let web_pull = seen
		.iter()
		.position(|r| r.contains("/images/pull") && r.contains("img-web"))
		.expect("expected web's image to be pulled");
	assert!(
		db_pull < first_create,
		"db's image must be prefetched before any container is created: {seen:?}"
	);
	assert!(
		web_pull < first_create,
		"web's (level 2) image must be prefetched up front too, not deferred behind level 1's barrier: {seen:?}"
	);
}

/// #6.2 regression: `up --scale web=N` used to create+start every replica in
/// strict sequence, paying N x (create+start) serially. All three replicas of
/// a scaled service must now be created and started, regardless of the fan-out
/// becoming concurrent.
#[tokio::test]
#[cfg(unix)]
async fn up_creates_and_starts_every_scaled_replica() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "POST" && target.contains("/containers/create") {
			(200, "{}".to_string())
		} else if method == "POST" && target.contains("/start") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str("services:\n  web:\n    image: img\n    scale: 3\n").unwrap();

	e.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect("a healthy scaled-up must succeed");

	let seen = fake.requests.lock().unwrap();
	let creates = seen
		.iter()
		.filter(|r| r.contains("POST") && r.contains("/containers/create"))
		.count();
	assert_eq!(creates, 3, "every replica must be created: {seen:?}");
	for i in 1..=3 {
		assert!(
			seen.iter()
				.any(|r| r.contains(&format!("proj-web-{i}/start"))),
			"expected replica {i} to be started: {seen:?}"
		);
	}
}

/// A genuine per-replica create/start failure must still surface as `Err`
/// (not be swallowed into an exit-0 `up`), and — since replicas 1 and 3 both
/// fail with distinct statuses — the reported error must deterministically be
/// replica 1's (earliest in the fixed replica-index order `join_bounded`
/// preserves), never replica 3's, regardless of which one's future actually
/// completes first. Every replica must still have been attempted: a failing
/// replica must not stop the others from being created/started.
#[tokio::test]
#[cfg(unix)]
async fn up_replica_fanout_surfaces_deterministic_first_error_after_attempting_the_rest() {
	let fake = fake_podman::start(|method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, "[]".to_string())
		} else if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "POST" && target.contains("/containers/create") {
			(200, "{}".to_string())
		} else if method == "POST" && target.contains("proj-web-1/start") {
			(500, r#"{"message":"replica 1 boom"}"#.to_string())
		} else if method == "POST" && target.contains("proj-web-3/start") {
			(503, r#"{"message":"replica 3 boom"}"#.to_string())
		} else if method == "POST" && target.contains("/start") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str("services:\n  web:\n    image: img\n    scale: 3\n").unwrap();

	let err = e
		.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect_err("a genuine replica start failure must propagate, not exit 0");
	assert!(
		matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
		"expected replica 1's (first, index order) failure, not replica 3's: {err:?}"
	);

	// Best-effort: every replica must still have been attempted despite two
	// of them failing.
	let seen = fake.requests.lock().unwrap();
	for i in 1..=3 {
		assert!(
			seen.iter()
				.any(|r| r.contains(&format!("proj-web-{i}/start"))),
			"expected replica {i}'s start to have been attempted: {seen:?}"
		);
	}
}

/// The per-replica `no_recreate` skip logic must survive the concurrent
/// fan-out: an already-present replica is left alone (`ensure_started`, no
/// create) while its sibling — not yet present — is still created and
/// started, matching what the pre-parallel serial loop did for each replica
/// in turn.
#[tokio::test]
#[cfg(unix)]
async fn up_replica_fanout_preserves_the_no_recreate_skip_per_replica() {
	let containers = r#"[{"Names":["/proj-web-1"]}]"#;
	let fake = fake_podman::start(move |method, target| {
		if method == "GET" && target.contains("/containers/json") {
			(200, containers.to_string())
		} else if method == "POST" && target.contains("/images/pull") {
			(200, String::new())
		} else if method == "POST" && target.contains("/containers/create") {
			(200, "{}".to_string())
		} else if method == "POST" && target.contains("/start") {
			(200, String::new())
		} else {
			(404, r#"{"message":"not found"}"#.to_string())
		}
	});
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str("services:\n  web:\n    image: img\n    scale: 2\n").unwrap();

	// `no_recreate = true` (docker compose create / the `scale` path): an
	// already-present replica is left in place instead of being recreated.
	e.up_with_options(&file, false, &[], &[], true, false, false)
		.await
		.expect("a partially-existing scaled up must succeed");

	let seen = fake.requests.lock().unwrap();
	let creates = seen
		.iter()
		.filter(|r| r.contains("POST") && r.contains("/containers/create"))
		.count();
	assert_eq!(
		creates, 1,
		"only replica 2 (not yet present) should be created; replica 1 is skipped: {seen:?}"
	);
	assert!(
		seen.iter().any(|r| r.contains("proj-web-1/start")),
		"the already-present replica 1 must still be ensured started: {seen:?}"
	);
	assert!(
		seen.iter().any(|r| r.contains("proj-web-2/start")),
		"the newly-created replica 2 must be started: {seen:?}"
	);
}

#[test]
fn rm_path_omits_volume_flag_by_default() {
	// A plain `down` (or scale-down) must not drop volumes.
	let path = container_rm_path("proj-web-1", false);
	assert!(path.ends_with("/proj-web-1?force=true"), "got: {path}");
	assert!(!path.contains("v=true"), "got: {path}");
}

#[test]
fn rm_path_requests_anonymous_volume_removal() {
	// `down -v` must pass `v=true` so podman reclaims the container's
	// anonymous (image VOLUME / short-form) volumes.
	let path = container_rm_path("proj-web-1", true);
	assert!(path.contains("force=true"), "got: {path}");
	assert!(path.contains("&v=true"), "got: {path}");
}

#[test]
fn rm_path_url_encodes_container_name() {
	// Names are URL-encoded so a slash in a container name cannot alter the
	// request path.
	let path = container_rm_path("weird/name", true);
	assert!(!path.contains("weird/name"), "got: {path}");
	assert!(path.contains("weird%2Fname"), "got: {path}");
}

/// `down --rmi` used to warn and return Ok on a real removal failure, so it
/// reported success having left images behind — the one arm of this teardown
/// that did not aggregate, while its network, volume and container siblings all
/// did.
#[cfg(unix)]
#[tokio::test]
async fn remove_service_images_reports_a_real_failure_after_trying_them_all() {
	let attempted = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
	let seen = std::sync::Arc::clone(&attempted);
	let fake = fake_podman::start(move |method, target| {
		if method == "DELETE" && target.contains("/images/") {
			seen.lock().expect("lock").push(target.to_string());
			// A 500 that is neither 404 nor an in-use conflict is a real failure.
			if target.contains("broken") {
				return (500, r#"{"message":"internal error"}"#.to_string());
			}
			return (200, String::new());
		}
		(404, r#"{"message":"not found"}"#.to_string())
	});
	let engine = engine_with(fake.client(), "proj");

	let mut file = ComposeFile::default();
	file.services.insert(
		"a".into(),
		Service {
			image: Some("broken:latest".into()),
			..Default::default()
		},
	);
	file.services.insert(
		"b".into(),
		Service {
			image: Some("fine:latest".into()),
			..Default::default()
		},
	);

	let err = engine
		.remove_service_images(&file, false)
		.await
		.expect_err("a real image-removal failure must propagate");
	assert!(
		matches!(err, ComposeError::Podman(ref pe) if pe.is_status(500)),
		"got: {err}"
	);
	assert_eq!(
		attempted.lock().expect("lock").len(),
		2,
		"the sweep must finish before reporting: both images are attempted"
	);
}
