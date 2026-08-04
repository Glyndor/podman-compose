//! Service-name resolution between siblings, split by layer (#1330).
//!
//! Reaching a service by its compose name needs three things to work, and only
//! the first belongs to podup:
//!
//! 1. podup registers the service name as a network alias,
//! 2. netavark wires the network,
//! 3. aardvark-dns answers the lookup.
//!
//! These tests used to assert only the end-to-end outcome, so a runtime whose
//! DNS server had died reported `service \`server\` was not reachable by its
//! service name` — which reads as a podup defect and cost real debugging time
//! before measurement showed aardvark-dns was simply not running. Each layer is
//! now asserted on its own, and a failure names the layer that produced it.

use super::*;

/// Ask whether the container runtime's DNS is answering at all.
///
/// Only ever called after a lookup has already failed, so a passing test pays
/// nothing for it.
///
/// The probe is the *container's own name*, which the runtime registers itself
/// with no involvement from podup. That makes it a clean discriminator: a name
/// podup never touched failing to resolve cannot be podup's alias handling.
///
/// The signatures come from the measurement in #1330 rather than from guessing
/// at what busybox prints — a dead server times out, while a live server that
/// does not know the name answers NXDOMAIN, and only the first is the runtime's
/// fault. `test_exec_capture` attaches stderr and does not inspect the exit
/// code, so a failed lookup arrives as `Ok` carrying its own complaint.
async fn runtime_dns_is_down(engine: &Engine, from: &str, own_name: &str) -> bool {
	match engine
		.test_exec_capture(from, vec!["nslookup".into(), own_name.into()])
		.await
	{
		Ok(out) => {
			out.contains("no servers could be reached") || out.contains("connection timed out")
		}
		// The exec failing says nothing about DNS. Staying quiet here keeps the
		// original assertion's message, which is the honest one when the cause
		// is unknown.
		Err(_) => false,
	}
}

/// The body both tests share: bring the project up, assert podup's layer, then
/// the runtime's, and tear down whatever happened.
///
/// `alias_context` distinguishes the two topologies in the failure message —
/// an explicit `networks:` block versus the synthesized `default` network —
/// because "the alias is missing" has a different cause in each.
async fn assert_sibling_resolves_by_service_name(
	engine: &Engine,
	file: &podup::compose::types::ComposeFile,
	proj: &str,
	alias_context: &str,
) {
	let server = format!("{proj}-server-1");
	let client = format!("{proj}-client-1");

	engine.up(file).await.unwrap();

	// **podup's layer**, checkable with no DNS involved: the compose service
	// name is registered as a network alias. This is the whole of podup's
	// contribution to service-name resolution, and the part a podup regression
	// would break.
	let aliases = engine
		.test_container_aliases(&server)
		.await
		.expect("could not read the server's network aliases");

	// **The runtime's layer**: the alias actually answers. Retry briefly while
	// the server's httpd comes up.
	let out = engine
		.test_exec_capture(
			&client,
			vec![
				"sh".into(),
				"-c".into(),
				"for i in $(seq 1 30); do wget -q -O - http://server:80/ && exit 0; sleep 0.3; done; exit 1".into(),
			],
		)
		.await;

	// Only probe DNS when the lookup did not answer, and do it before `down`
	// removes the containers the probe needs.
	let dns_down = match &out {
		Ok(o) if o.contains("ok") => false,
		_ => runtime_dns_is_down(engine, &client, &server).await,
	};

	engine.down(file).await.unwrap();

	assert!(
		aliases.iter().any(|a| a == "server"),
		"podup did not register the service name as a network alias {alias_context}: {aliases:?}"
	);

	// A runtime whose DNS is down cannot answer this question, and a test that
	// could not run is not a test that failed. It skips — but only where the
	// environment does not promise Podman works.
	//
	// Where it does (the nested-virt lane sets PODUP_REQUIRE_PODMAN), the skip
	// becomes a hard failure, for the same reason `podman()` refuses to skip
	// there: a lane that reports `ok` for tests it never ran is the failure mode
	// this whole mechanism exists to prevent. The message names the runtime so
	// the next reader does not start by suspecting podup.
	if dns_down {
		assert!(
			std::env::var_os("PODUP_REQUIRE_PODMAN").is_none(),
			"the alias `server` is registered, so podup did its part, but the container \
			 runtime's DNS server did not answer a lookup for the container's own name \
			 either — aardvark-dns is down. PODUP_REQUIRE_PODMAN is set, so this is a \
			 broken environment rather than a test to skip."
		);
		eprintln!(
			"skipping: the container runtime's DNS is not answering (aardvark-dns), so \
			 service-name resolution cannot be measured here"
		);
		return;
	}

	let out = out.expect("exec in client container failed");
	assert!(
		out.contains("ok"),
		"the alias `server` is registered and the runtime's DNS is answering, so the \
		 lookup failing is a real service-name resolution defect: {out:?}"
	);
}

// ---------------------------------------------------------------------------
// A sibling resolves a service by its service name on a shared network
// ---------------------------------------------------------------------------

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn sibling_resolves_service_by_name_on_shared_network() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("dns");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  server:\n    image: busybox:latest\n    command: [\"sh\", \"-c\", \"mkdir -p /www; echo ok > /www/index.html; exec httpd -f -p 80 -h /www\"]\n    networks:\n      - appnet\n  client:\n    image: busybox:latest\n    command: [\"sleep\", \"infinity\"]\n    networks:\n      - appnet\nnetworks:\n  appnet:\n",
	)
	.unwrap();

	assert_sibling_resolves_by_service_name(&engine, &file, &proj, "on the shared network").await;
}

// ---------------------------------------------------------------------------
// With NO `networks:` block, services still reach each other by service name
// (the synthesized `default` network — docker-compose parity, #417)
// ---------------------------------------------------------------------------

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn sibling_resolves_service_by_name_without_networks_block() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("dnsdef");
	let engine = Engine::new(client, proj.clone());

	// No top-level `networks:` and no per-service `networks:` — the common case.
	// Parse through the real CLI entry point so the implicit `default` network
	// is synthesized; `parse_str` deliberately does not normalize.
	let dir = tempfile::tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		"services:\n  server:\n    image: busybox:latest\n    command: [\"sh\", \"-c\", \"mkdir -p /www; echo ok > /www/index.html; exec httpd -f -p 80 -h /www\"]\n  client:\n    image: busybox:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let file = parse_files_with_env_files(&[compose], &[]).unwrap();

	assert_sibling_resolves_by_service_name(
		&engine,
		&file,
		&proj,
		"on the synthesized default network",
	)
	.await;
}
