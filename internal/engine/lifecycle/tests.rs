#[cfg(unix)]
use super::Engine;
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
