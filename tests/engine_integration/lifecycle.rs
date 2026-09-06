//! Engine integration tests (split for the source line limit).
use super::*;

// Lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn up_and_down() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("udn");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let running = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let removed = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();

	assert_eq!(
		running,
		vec![format!("{proj}-web-1")],
		"up returned success without creating the service container"
	);
	assert!(
		removed.is_empty(),
		"down returned success and left containers behind: {removed:?}"
	);
}

#[tokio::test]
async fn up_no_recreate_skips_running() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("nor");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// Mark the container's filesystem. A recreate builds a fresh one under the
	// same name, so the marker is what separates "skipped the running container"
	// from "replaced it with an identical-looking one".
	engine
		.test_exec_capture(
			&cname,
			vec![
				"sh".into(),
				"-c".into(),
				"echo first-container > /marker".into(),
			],
		)
		.await
		.unwrap();

	// The second up carries a CHANGED config, which is the only shape that tests
	// the flag. With the config unchanged, a plain up skips the recreate on its
	// own because the config hash matches, so no_recreate and the hash produce
	// the same container and no assertion can separate them: a mutation that
	// ignored no_recreate entirely left the earlier version of this test green.
	let changed = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"120\"]\n",
	)
	.unwrap();
	engine
		.up_with_options(&changed, false, &[], &[], true, false, false, false)
		.await
		.unwrap();
	let with_flag = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/marker".into()])
		.await
		.unwrap_or_default();

	// Control, in the sense the testing standard asks for: the same changed
	// config without the flag must recreate. Without this, "the marker survived"
	// could just mean podup never noticed the config had changed, and the test
	// would pass while proving nothing about no_recreate.
	engine.up(&changed).await.unwrap();
	let without_flag = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/marker".into()])
		.await
		.unwrap_or_default();
	engine.down(&changed).await.unwrap();

	assert_eq!(
		with_flag.trim(),
		"first-container",
		"no_recreate recreated the container even though the flag was set"
	);
	assert_ne!(
		without_flag.trim(),
		"first-container",
		"the changed config did not recreate without the flag, so the fixture cannot prove the flag did anything"
	);
}

#[tokio::test]
async fn up_target_services_only() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("tgt");
	let engine = Engine::new(client, proj.clone());
	// `cache` exists so the targeting has something to leave out. With only db and
	// web declared, starting "web and its dependency" and starting the whole file
	// produce the same containers, so no assertion could tell the two apart.
	let file = parse_str(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    depends_on:\n      - db\n  cache:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	// Only start web (and its dep db)
	engine
		.up_with_options(
			&file,
			false,
			&[],
			&["web".to_string()],
			false,
			false,
			false,
			false,
		)
		.await
		.unwrap();
	let mut names = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();
	names.sort();
	engine.down(&file).await.unwrap();

	assert_eq!(
		names,
		vec![format!("{proj}-db-1"), format!("{proj}-web-1")],
		"targeting web must start web and its dependency db, and leave cache alone"
	);
}

#[tokio::test]
async fn down_with_remove_volumes() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("dvol");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(&format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - {proj}-data:/data\nvolumes:\n  {proj}-data:\n"
	))
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.down_with_options(&file, true).await.unwrap();
	// `down --volumes` has to take the volume with it. Whether it exists is not
	// something the library returns, so read it the way the sibling tests read
	// container config: out of band, through the podman CLI.
	let volumes = String::from_utf8_lossy(
		&std::process::Command::new("podman")
			.args(["volume", "ls", "--format", "{{.Name}}"])
			.output()
			.expect("podman volume ls")
			.stdout,
	)
	.to_string();

	// Match on the project prefix, not the declared name. podup namespaces a
	// declared volume as `{project}_{name}`, so an equality check against
	// `{proj}-data` can never match and the assertion would hold whatever `down`
	// did, which is how the first version of this line survived its mutation.
	assert!(
		!volumes.lines().any(|v| v.contains(proj.as_str())),
		"down --volumes left a volume belonging to the project behind: {volumes:?}"
	);
}

#[tokio::test]
async fn restart_all_services() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rsa");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	let started = poll_container_file(&engine, &cname, "/starts", "start", 30).await;

	engine.restart(&file, None).await.unwrap();
	let restarted = poll_container_file(&engine, &cname, "/starts", "start\nstart", 60).await;

	engine.down(&file).await.unwrap();
	assert!(started, "the container never wrote its first start line");
	assert!(
		restarted,
		"restart returned success but the process never started a second time"
	);
}

#[tokio::test]
async fn restart_specific_service() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rss");
	let engine = Engine::new(client, proj.clone());
	// Two services, so naming one is a choice the assertion can check. Restarting
	// everything would satisfy a test that only looked at web.
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n  idle:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let web = format!("{proj}-web-1");
	let idle = format!("{proj}-idle-1");
	let started = poll_container_file(&engine, &web, "/starts", "start", 30).await;

	engine.restart(&file, Some("web")).await.unwrap();
	let restarted = poll_container_file(&engine, &web, "/starts", "start\nstart", 60).await;
	let untouched = engine
		.test_exec_capture(&idle, vec!["cat".into(), "/starts".into()])
		.await
		.unwrap_or_default();

	engine.down(&file).await.unwrap();
	assert!(started, "web never wrote its first start line");
	assert!(
		restarted,
		"restart returned success but web never started a second time"
	);
	assert_eq!(
		untouched.trim(),
		"start",
		"restarting web also restarted idle, which was not named"
	);
}

#[tokio::test]
async fn restart_cascade_dep() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("rcd");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  db:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n  web:\n    image: alpine:latest\n    command: [\"sh\", \"-c\", \"echo start >> /starts; sleep infinity\"]\n    depends_on:\n      db:\n        condition: service_started\n        restart: true\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let db = format!("{proj}-db-1");
	let web = format!("{proj}-web-1");
	let started = poll_container_file(&engine, &web, "/starts", "start", 30).await;

	// Restarting db triggers cascade restart of web
	engine.restart(&file, Some("db")).await.unwrap();
	let db_restarted = poll_container_file(&engine, &db, "/starts", "start\nstart", 60).await;
	let web_cascaded = poll_container_file(&engine, &web, "/starts", "start\nstart", 60).await;

	engine.down(&file).await.unwrap();
	assert!(started, "web never wrote its first start line");
	assert!(db_restarted, "the named service db never restarted");
	assert!(
		web_cascaded,
		"db restarted but the restart: true dependant web did not follow"
	);
}

#[tokio::test]
async fn restart_unknown_service_fails() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("ruf");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let err = engine
		.restart(&file, Some("nonexistent"))
		.await
		.unwrap_err();
	assert!(matches!(err, podup::ComposeError::ServiceNotFound(_)));
}
