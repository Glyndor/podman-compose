use std::collections::HashSet;
use std::sync::Arc;

use super::unshare_readiness_error;
use crate::engine::Engine;
use crate::error::ComposeError;
use crate::libpod::Client;

fn engine(project: &str) -> Engine {
	// The map is built without any socket call (the shared futures are lazy),
	// so a client bound to a never-opened path is enough — no runtime needed.
	let client = Client::new("/tmp/podup-readiness-test.sock");
	Engine::with_base_dir(client, project.into(), std::env::temp_dir())
}

fn enabled_all(file: &crate::compose::types::ComposeFile) -> HashSet<String> {
	file.services.keys().cloned().collect()
}

#[test]
fn shares_one_poller_per_service_healthy_container() {
	// web and api both wait on db with `service_healthy`; cache is waited on
	// with `service_started` (never polled). Exactly one shared entry — db's
	// container — must result, not one per dependent.
	let yaml = "\
services:
  db:
    image: x
    healthcheck:
      test: [\"CMD\", \"true\"]
  cache:
    image: x
  web:
    image: x
    depends_on:
      db:
        condition: service_healthy
      cache:
        condition: service_started
  api:
    image: x
    depends_on:
      db:
        condition: service_healthy
";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine("proj");
	let map = e.build_readiness_map(&file, &enabled_all(&file), &None, true);
	let keys: Vec<&String> = map.keys().collect();
	assert_eq!(map.len(), 1, "one shared poller expected, got {keys:?}");
	assert!(
		keys[0].contains("db"),
		"shared container should be db, got {keys:?}"
	);
}

#[test]
fn create_only_shares_nothing() {
	// `create` (start = false) gates on no dependency, so nothing is shared.
	let yaml = "\
services:
  db:
    image: x
    healthcheck:
      test: [\"CMD\", \"true\"]
  web:
    image: x
    depends_on:
      db:
        condition: service_healthy
";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine("proj");
	assert!(e
		.build_readiness_map(&file, &enabled_all(&file), &None, false)
		.is_empty());
}

#[test]
fn sharing_a_poller_preserves_the_error_variant() {
	// Regression guard for the public error contract: sharing the poller must
	// not change which variant `up()` returns. Both reconstructible causes are
	// asserted by variant, not by message — the wrapper displays transparently,
	// so a message assertion would have passed while the contract was broken.
	let timeout = Arc::new(ComposeError::HealthCheckTimeout("db-1".into()));
	assert!(matches!(
		unshare_readiness_error(&timeout),
		ComposeError::HealthCheckTimeout(c) if c == "db-1"
	));

	let exited = Arc::new(ComposeError::WaitServiceExited {
		container: "db-1".into(),
		code: 3,
	});
	assert!(matches!(
		unshare_readiness_error(&exited),
		ComposeError::WaitServiceExited { container, code } if container == "db-1" && code == 3
	));
}

#[test]
fn a_non_reconstructible_cause_keeps_the_transparent_wrapper() {
	// `ComposeError::Podman` holds a non-`Clone` payload, so it cannot be
	// rebuilt; it stays wrapped, and `innermost()` is what peels it.
	let podman = Arc::new(ComposeError::Podman(crate::libpod::PodmanError::Api {
		status: 500,
		message: "boom".into(),
	}));
	let out = unshare_readiness_error(&podman);
	assert!(matches!(out, ComposeError::DependencyNotReady(_)));
	assert!(matches!(out.innermost(), ComposeError::Podman(_)));
}

#[test]
fn disabled_healthcheck_is_not_shared() {
	// A dependency whose healthcheck is disabled is treated as satisfied, so it
	// is never polled and must not get a shared poller.
	let yaml = "\
services:
  db:
    image: x
    healthcheck:
      disable: true
  web:
    image: x
    depends_on:
      db:
        condition: service_healthy
";
	let file = crate::compose::parse_str(yaml).unwrap();
	let e = engine("proj");
	assert!(
		e.build_readiness_map(&file, &enabled_all(&file), &None, true)
			.is_empty(),
		"a disabled healthcheck must not be shared or polled"
	);
}
