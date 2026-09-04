use crate::engine::fake_podman;
use crate::engine::Engine;
use crate::ui::progress::capture::Capture;

fn engine_with(client: crate::libpod::Client, project: &str) -> Engine {
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

/// A libpod stand-in for a host that already has every image asked about — the
/// warm state the pull-skip decision turns on. Shared by the three tests that
/// exercise that decision, so they cannot drift apart on what "already here"
/// means.
fn present_image_engine(method: &str, target: &str) -> (u16, String) {
	if method == "POST" && target.contains("/images/pull") {
		(200, String::new())
	} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
		(200, r#"{"Id":"sha256:cafe"}"#.to_string())
	} else if method == "GET" && target.contains("/containers/json") {
		(200, "[]".to_string())
	} else if method == "POST" && target.contains("/containers/create") {
		(200, "{}".to_string())
	} else if method == "POST" && target.contains("/start") {
		(200, String::new())
	} else {
		(404, r#"{"message":"not found"}"#.to_string())
	}
}

/// A warm `up` must not pull an image the host already has, once per service.
///
/// The prefetch stage checks presence once and returns without pulling, and then
/// `acquire_service_image` pulled anyway, per service, under the effective
/// `missing` policy — 42 of the 88 requests a 42-service warm `up` issued, and a
/// `Pulling` line on the user's terminal for each. docker compose against the
/// same engine prints none.
///
/// Nothing in the suite counted the pull requests an `up` issues, which is how
/// that survived; this is that count.
#[tokio::test]
async fn warm_up_does_not_pull_an_image_the_host_already_has() {
	let fake = fake_podman::start(present_image_engine);
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str(
		"services:\n  a:\n    image: shared\n  b:\n    image: shared\n  c:\n    image: shared\n",
	)
	.unwrap();

	e.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect("a warm up on a present image must succeed");

	let seen = fake.requests.lock().unwrap();
	let pulls = seen.iter().filter(|r| r.contains("/images/pull")).count();
	assert_eq!(
		pulls, 0,
		"three services sharing an image the host already has must pull it zero times, not once each: {seen:?}"
	);
}

/// The skip is bounded by the `missing` policy. `always` means go to the
/// registry whatever is local, and widening the skip to it would bring back
/// #1076: libpod reports a failed pull as an in-band line on a 200, so a pull
/// that never happens is a failure line that can never be read.
///
/// The two services share one image on purpose, with different policies. With
/// `always` alone the prefetch stage never records the image as present, so the
/// skip could not fire whatever the policy check said, and the test would pass
/// for the wrong reason — it did, until a mutation run showed it surviving the
/// removal of the very guard it is named for. The `missing` service is what
/// records the observation, which is the only state where that guard is the one
/// thing standing between `always` and a skipped registry visit.
#[tokio::test]
async fn an_always_policy_still_pulls_an_image_another_service_saw_present() {
	let fake = fake_podman::start(present_image_engine);
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str(
		"services:\n  a:\n    image: shared\n  b:\n    image: shared\n    pull_policy: always\n",
	)
	.unwrap();

	e.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect("an always-policy up must succeed");

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter().any(|r| r.contains("/images/pull")),
		"an always policy must still reach the registry even when a sibling service saw the image locally: {seen:?}"
	);
}

/// The skip never applies to a service pinning `platform:`. Presence is matched
/// on the image reference, which carries no architecture, so honouring an
/// observation there could start the wrong variant.
#[tokio::test]
async fn a_platform_pinned_service_still_pulls_an_image_the_host_already_has() {
	let fake = fake_podman::start(present_image_engine);
	let e = engine_with(fake.client(), "proj");

	let file = crate::parse_str(
		"services:\n  a:\n    image: shared\n  b:\n    image: shared\n    platform: linux/arm64\n",
	)
	.unwrap();

	e.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect("a platform-pinned up must succeed");

	let seen = fake.requests.lock().unwrap();
	assert!(
		seen.iter().any(|r| r.contains("/images/pull")),
		"a platform-pinned service must still pull: a reference match says nothing about the architecture that is local: {seen:?}"
	);
}

/// A typo'd `pull_policy:` on a warm service must error at the
/// `image_already_seen_present` decision (the `#1443` site) rather than
/// be silently treated as `missing` and skip the only pull that would have
/// surfaced the bad value. The host stands in for one that already has the
/// image (so the skip branch *would* fire), and the assertion is that the
/// error fires instead — without the fix the call returned `true` and the
/// pull was skipped, leaving `up` to exit 0 with the wrong image.
#[tokio::test]
async fn image_already_seen_present_rejects_an_unknown_pull_policy() {
	let e = engine_with(crate::libpod::Client::new("/nonexistent.sock"), "proj");
	// Pre-populate the prefetch's seen-present set as if a previous
	// `prefetch_images` had confirmed the image on this host. Without the
	// fix `image_already_seen_present` would then return `true` for any
	// service whose effective policy it read as `missing` — including a
	// typo'd `pull_policy:` — and the only pull that could have surfaced
	// the bad value would be skipped.
	e.images_seen_present
		.lock()
		.unwrap()
		.insert("shared".to_string());

	let file = crate::parse_str("services:\n  web:\n    image: shared\n    pull_policy: alaways\n")
		.unwrap();
	let err = e
		.image_already_seen_present("web", &file.services["web"])
		.expect_err("an unknown pull_policy must error at the skip decision");
	assert!(
		matches!(
			err,
			crate::error::ComposeError::Podman(crate::libpod::PodmanError::Field {
				ref service,
				ref field,
				ref value,
				..
			}) if service == "web" && field == "pull_policy" && value == "alaways"
		),
		"unknown pull_policy must surface as a Field error naming the offending service and value, got {err:?}"
	);
}

/// `up` against a service whose image is missing builds it as part of the
/// `up` board, not before it. The image row appears inside the same board
/// `up` already drew, with the working verb `Building` and the closing
/// verb `Built`, before the container rows that depend on it. Before
/// #1681 the stream landed before the board started, so nothing said
/// which service was being built or how long it took.
#[tokio::test]
async fn up_with_a_missing_image_builds_on_the_same_board() {
	let chunks = vec![
		"{\"stream\":\"STEP 1/2: FROM docker.io/library/alpine:3.20\\n\"}\n".to_string(),
		"{\"stream\":\"--> 3f3c8b769775\\n\"}\n".to_string(),
		"{\"stream\":\"STEP 2/2: CMD [\\\"echo\\\",\\\"hi\\\"]\\n\"}\n".to_string(),
		"{\"stream\":\"--> 9f3c8b769775\\n\"}\n".to_string(),
		"{\"stream\":\"COMMIT localhost/ux-up-build:1\\n\"}\n".to_string(),
		"{\"stream\":\"--> sha256:1111111111111111111111111111111111111111111111111111111111111111\\n\"}\n".to_string(),
		"{\"stream\":\"Successfully tagged localhost/ux-up-build:1\\n\"}\n".to_string(),
	];
	// The image is missing locally (the inspect probe returns 404), so
	// `acquire_service_image` decides to build. The fake answers `/build`
	// with the canned stream and answers the post-build `/tag` call so the
	// extra-tags step succeeds; everything else 404s so the test can read
	// what was actually called.
	let fake = crate::engine::fake_podman::start_replying(move |method, target| {
		if method == "POST" && target.contains("/build?") {
			crate::engine::fake_podman::FakeReply::ChunkedEnd(chunks.clone())
		} else if method == "POST" && target.contains("/images/") && target.contains("/tag") {
			crate::engine::fake_podman::FakeReply::Body(200, String::new())
		} else if method == "GET" && target.contains("/images/") && target.contains("/json") {
			// Image absent: the missing-policy branch takes the build path.
			crate::engine::fake_podman::FakeReply::Body(
				404,
				r#"{"message":"no such image"}"#.to_string(),
			)
		} else if method == "GET" && target.contains("/containers/json") {
			crate::engine::fake_podman::FakeReply::Body(200, "[]".to_string())
		} else if method == "POST" && target.contains("/containers/create") {
			crate::engine::fake_podman::FakeReply::Body(200, "{}".to_string())
		} else if method == "POST" && target.contains("/start") {
			crate::engine::fake_podman::FakeReply::Body(200, String::new())
		} else {
			crate::engine::fake_podman::FakeReply::Body(
				404,
				r#"{"message":"not found"}"#.to_string(),
			)
		}
	});

	// A real context directory on disk: `acquire_service_image` calls
	// `build_service`, which inspects the path before posting.
	let ctx = tempfile::tempdir().expect("tempdir");
	std::fs::write(
		ctx.path().join("Dockerfile"),
		b"FROM docker.io/library/alpine:3.20\nCMD [\"echo\",\"hi\"]\n",
	)
	.expect("write Dockerfile");
	let mut engine = Engine::with_base_dir(fake.client(), "proj".into(), ctx.path().to_path_buf());
	engine.no_warn = true;
	let compose = crate::parse_str(
		"services:\n  app:\n    image: localhost/ux-up-build:1\n    build:\n      context: .\n    command: [\"echo\",\"hi\"]\n",
	)
	.unwrap();

	let capture = Capture::start();
	engine
		.up_with_options(&compose, true, &[], &[], false, false, false)
		.await
		.expect("an up that builds its missing image succeeds");

	// The image row's `start_anchored` lands before the container row's
	// first event, which is the order signal the renderer sees: an `up`
	// that builds its missing image paints the image row above the
	// container rows that depend on it. We rely on Capture's per-thread
	// event log rather than the global session so this test stays
	// independent of the progress-enabled toggle.
	let verbs = capture.verbs();
	let first_image_idx = verbs
		.iter()
		.position(|(_, n, _)| n == "localhost/ux-up-build:1");
	let first_container_idx = verbs.iter().position(|(_, n, _)| n == "proj-app-1");
	assert!(
		first_image_idx.is_some() && first_container_idx.is_some(),
		"both rows see events on the up board: {verbs:?}"
	);
	assert!(
		first_image_idx.unwrap() < first_container_idx.unwrap(),
		"the image row's first event is before the container row's first: {verbs:?}"
	);
	assert!(
		verbs
			.iter()
			.any(|(_, n, v)| n == "localhost/ux-up-build:1" && v == "Building")
			&& verbs
				.iter()
				.any(|(_, n, v)| n == "localhost/ux-up-build:1" && v == "Built"),
		"the image row went Building -> Built on the same board: {verbs:?}"
	);
	assert!(
		capture.every_board_ended(),
		"the board closes on the way out, even with a build"
	);
}
