use super::*;
use crate::compose::parse_str;

/// `up_resources` never touches the client — it reads the compose file and the
/// project name — but an `Engine` needs one, so the tests borrow the same fake
/// the rest of the lifecycle suite uses. That fake is a unix socket, which is
/// why this module is `cfg(unix)`.
fn engine(fake: &crate::engine::fake_podman::FakePodman) -> Engine {
	Engine::with_base_dir(fake.client(), "proj".into(), std::env::temp_dir())
}

async fn seeded(yaml: &str, targets: &[&str]) -> Vec<(Kind, String)> {
	let file = parse_str(yaml).unwrap();
	let enabled: HashSet<String> = file.services.keys().cloned().collect();
	let set = (!targets.is_empty()).then(|| {
		targets
			.iter()
			.map(|s| (*s).to_string())
			.collect::<HashSet<_>>()
	});
	let fake = crate::engine::fake_podman::start(|_, _| (404, "{}".to_string()));
	engine(&fake).up_resources(&file, &enabled, &set)
}

const TWO_SERVICES: &str = "\
services:
  db:
    image: alpine
  web:
    image: alpine
    depends_on:
      - db
volumes:
  data:
networks:
  extra:
";

/// Networks and volumes come first, then containers: that is the order the work
/// happens in, and the board's completed rows scroll away in the same order.
#[tokio::test]
async fn resources_are_seeded_in_the_order_the_work_happens() {
	let out = seeded(TWO_SERVICES, &[]).await;
	let kinds: Vec<Kind> = out.iter().map(|(k, _)| *k).collect();
	let first_container = kinds.iter().position(|k| *k == Kind::Container).unwrap();
	assert!(
		kinds[..first_container]
			.iter()
			.all(|k| matches!(k, Kind::Network | Kind::Volume)),
		"{kinds:?}"
	);
}

/// Containers follow the dependency graph, so the board predicts the order the
/// user will actually see them start in.
#[tokio::test]
async fn containers_follow_the_dependency_order() {
	let out = seeded(TWO_SERVICES, &[]).await;
	let names: Vec<&str> = out
		.iter()
		.filter(|(k, _)| *k == Kind::Container)
		.map(|(_, n)| n.as_str())
		.collect();
	assert_eq!(names, vec!["proj-db-1", "proj-web-1"]);
}

/// podup never creates an external network or volume, so it is not work this
/// command will do and must not sit on the board waiting to happen.
#[tokio::test]
async fn external_resources_are_not_seeded() {
	let yaml = "\
services:
  web:
    image: alpine
volumes:
  theirs:
    external: true
networks:
  shared:
    external: true
";
	let out = seeded(yaml, &[]).await;
	assert!(
		out.iter().all(|(k, _)| *k == Kind::Container),
		"external resources must not be seeded: {out:?}"
	);
}

/// A targeted `up web` seeds only what it will touch. Rows for services this
/// pass will never start would sit `Pending` forever.
#[tokio::test]
async fn a_targeted_up_seeds_only_its_targets() {
	let out = seeded(TWO_SERVICES, &["web"]).await;
	let names: Vec<&str> = out
		.iter()
		.filter(|(k, _)| *k == Kind::Container)
		.map(|(_, n)| n.as_str())
		.collect();
	assert_eq!(names, vec!["proj-web-1"]);
}

/// A scaled service gets one row per replica, because the board tracks
/// containers rather than services — that is the granularity every progress
/// event in the tree already reports at.
#[tokio::test]
async fn a_scaled_service_seeds_one_row_per_replica() {
	let yaml = "\
services:
  web:
    image: alpine
    deploy:
      replicas: 3
";
	let out = seeded(yaml, &[]).await;
	let names: Vec<&str> = out.iter().map(|(_, n)| n.as_str()).collect();
	assert_eq!(names, vec!["proj-web-1", "proj-web-2", "proj-web-3"]);
}

/// Volume and network names are the resolved on-host ones, not the compose
/// keys, so a row matches the event that will later arrive for it.
#[tokio::test]
async fn names_are_the_resolved_on_host_ones() {
	let out = seeded(TWO_SERVICES, &[]).await;
	let names: Vec<&str> = out.iter().map(|(_, n)| n.as_str()).collect();
	assert!(names.contains(&"proj_data"), "{names:?}");
	assert!(names.contains(&"proj_extra"), "{names:?}");
}
