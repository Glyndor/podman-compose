/// The `x-podman-autoupdate` extension reaches the container spec as
/// `io.containers.autoupdate=<value>` so `podman auto-update` can see the
/// container (#1656).
#[cfg(unix)]
#[tokio::test]
async fn x_podman_autoupdate_puts_the_label_on_the_container_spec() {
	use crate::engine::Engine;
	let fake = crate::engine::fake_podman::start(|method, target| {
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
	let client = fake.client();
	let e = Engine::with_base_dir(client, "proj".into(), std::env::temp_dir());
	let file =
		crate::parse_str("services:\n  web:\n    image: img\n    x-podman-autoupdate: registry\n")
			.unwrap();
	e.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect("a healthy up must succeed");

	let reqs = fake.requests.lock().unwrap();
	let bodies = fake.bodies.lock().unwrap();
	let create_body = reqs
		.iter()
		.zip(bodies.iter())
		.find(|(r, _)| r.contains("/containers/create"))
		.map(|(_, b)| b)
		.expect("POST /containers/create must appear with a body");
	let create_body = std::str::from_utf8(create_body).expect("body is utf-8");
	assert!(
		create_body.contains("\"io.containers.autoupdate\":\"registry\""),
		"the autoupdate label must appear on the spec: {create_body}"
	);
}

/// A bogus `x-podman-autoupdate` value is rejected at create time, the same
/// way `x-podman-on-failure` is, the operator wrote `always`, not `registry`
/// or `local`, and `podman auto-update` would never have honoured it.
#[cfg(unix)]
#[tokio::test]
async fn x_podman_autoupdate_rejects_a_bogus_value_at_create_time() {
	use crate::engine::Engine;
	let fake = crate::engine::fake_podman::start(|method, target| {
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
	let client = fake.client();
	let e = Engine::with_base_dir(client, "proj".into(), std::env::temp_dir());
	let file =
		crate::parse_str("services:\n  web:\n    image: img\n    x-podman-autoupdate: always\n")
			.unwrap();
	let err = e
		.up_with_options(&file, false, &[], &[], false, false, false)
		.await
		.expect_err("a bogus autoupdate value must be rejected at create time");
	let msg = err.to_string();
	assert!(
		msg.contains("always") && msg.contains("registry") && msg.contains("local"),
		"the rejection must name the offending value and the two allowed spellings: {msg}"
	);
}
