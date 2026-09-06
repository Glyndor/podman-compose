//! Engine integration tests (split for the source line limit).
use super::*;

// Configs: file and environment sources
// ---------------------------------------------------------------------------

#[tokio::test]
async fn file_config_bound() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let cfg_file = dir.path().join("app.conf");
	fs::write(&cfg_file, b"key=from-file").unwrap();

	let proj = proj("fcfg");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let yaml = format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    configs:\n      - filecfg\nconfigs:\n  filecfg:\n    file: {}\n",
		cfg_file.display()
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	// Read the config from inside the container. Asserting only that `up` and
	// `down` succeed passes just as happily when the config is missing, empty or
	// unreadable: the mount is not the effect, the content is.
	let read = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"test \"$(cat /filecfg)\" = key=from-file".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await;
	engine.down(&file).await.unwrap();
	assert!(
		read.is_ok(),
		"config /filecfg did not carry the file's contents: {read:?}"
	);
}

#[test]
fn env_config_materialized() {
	let rt = tokio::runtime::Runtime::new().unwrap();
	temp_env::with_var("PODUP_TEST_CFG_VAR", Some("cfg-from-env"), || {
		rt.block_on(async {
			let client = match podman().await {
				Some(d) => d,
				None => return,
			};
			let proj = proj("ecfg");
			let engine = Engine::new(client, proj.clone());
			let file = parse_str(
				"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    configs:\n      - envcfg\nconfigs:\n  envcfg:\n    environment: PODUP_TEST_CFG_VAR\n",
			)
			.unwrap();

			engine.up(&file).await.unwrap();
			let read = engine
				.exec_with_options(
					&file,
					"web",
					vec![
						"sh".to_string(),
						"-c".to_string(),
						"test \"$(cat /envcfg)\" = cfg-from-env".to_string(),
					],
					podup::ExecOptions::default(),
				)
				.await;
			engine.down(&file).await.unwrap();
			assert!(
				read.is_ok(),
				"config /envcfg did not carry the environment variable's value: {read:?}"
			);
		});
	});
}

// ---------------------------------------------------------------------------
// Container options: expose, deploy labels, annotations, tmpfs long-form
// ---------------------------------------------------------------------------

#[tokio::test]
async fn service_with_expose_deploy_labels_annotations_tmpfs() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("sdl");
	let engine = Engine::new(client, proj.clone());
	// expose covers container.rs L56-63
	// deploy.labels are accepted but, per the Compose Specification, are set on
	// the service only and must not be applied to the container
	// annotations covers container.rs L81-82
	// long-form tmpfs volume covers container.rs L107-113 and L139
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    expose:\n      - \"8080\"\n    annotations:\n      com.example.note: value\n    deploy:\n      labels:\n        com.example.env: test\n    volumes:\n      - type: tmpfs\n        target: /tmp/cache\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-web-1");
	// The tmpfs has to be a real tmpfs at that path, not a directory that happens
	// to exist. /proc/mounts is inside the container and needs no seam.
	let mounts = engine
		.test_exec_capture(
			&cname,
			vec!["grep".into(), " /tmp/cache ".into(), "/proc/mounts".into()],
		)
		.await
		.unwrap_or_default();
	// The comment above makes a falsifiable claim about deploy.labels, so check it
	// rather than leave it as prose. Labels and annotations are container config,
	// which the library does not return, so read them the way the CLI tests do.
	let inspect = |fmt: &str| {
		String::from_utf8_lossy(
			&std::process::Command::new("podman")
				.args(["inspect", &cname, "--format", fmt])
				.output()
				.expect("podman inspect")
				.stdout,
		)
		.trim()
		.to_string()
	};
	let deploy_label = inspect("{{index .Config.Labels \"com.example.env\"}}");
	let annotation = inspect("{{index .Config.Annotations \"com.example.note\"}}");
	engine.down(&file).await.unwrap();

	assert!(
		mounts.contains("tmpfs /tmp/cache tmpfs"),
		"the long-form tmpfs volume did not mount a tmpfs at /tmp/cache: {mounts:?}"
	);
	assert_eq!(
		deploy_label, "",
		"deploy.labels reached the container; the Compose Specification puts them on the service only"
	);
	assert_eq!(
		annotation, "value",
		"the annotation did not reach the container"
	);
}

// ---------------------------------------------------------------------------
// Volume: named volume with driver_opts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn named_volume_with_driver_opts() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("vdo");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	// driver_opts covers volume.rs L55 (Some(driver_opts) branch)
	// Use a bind-mount volume pointing to the temp dir (fast, rootless-safe)
	let yaml = format!(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - {proj}-cache:/cache:z\nvolumes:\n  {proj}-cache:\n    driver: local\n    driver_opts:\n      type: none\n      o: bind\n      device: {path}\n",
		path = dir.path().display()
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	// The `:z` relabel is not optional. This volume binds a host directory, and
	// on an SELinux-enforcing host the container is denied the write without it,
	// which is what reddened the Podman 5 leg of pull request #1278 while passing
	// on my machine, where SELinux is not enabled. The same trap is recorded in
	// .github/podman-known-failures-5 for run_flags::engine_run_applies_volume_
	// publish_and_interactive.
	//
	// The driver_opts bind the volume to this temp directory, so a file written
	// at /cache in the container has to appear on the host here. Without the opts
	// taking effect the container would still get a working /cache (an ordinary
	// local volume) and `up` would return Ok either way, which is exactly what
	// this test used to check.
	engine
		.test_exec_capture(
			&format!("{proj}-web-1"),
			vec![
				"sh".into(),
				"-c".into(),
				"echo bound > /cache/marker".into(),
			],
		)
		.await
		.unwrap();
	let on_host = fs::read_to_string(dir.path().join("marker")).unwrap_or_default();
	engine.down_with_options(&file, true).await.unwrap();

	assert_eq!(
		on_host.trim(),
		"bound",
		"driver_opts did not bind the volume to the host directory"
	);
}
