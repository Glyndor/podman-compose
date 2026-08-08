//! Integration tests for `label_file:` sanitization and the
//! `podup.config-files` URL-encoding (#1361).
use super::*;

// ---------------------------------------------------------------------------
// label_file: load labels from file
// ---------------------------------------------------------------------------

#[tokio::test]
async fn label_file_labels_applied() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(
		dir.path().join("svc.labels"),
		b"com.example.role=web\ncom.example.env=test\n",
	)
	.unwrap();
	let proj = proj("lfl");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    label_file: svc.labels\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let inspect = |key: &str| {
		String::from_utf8_lossy(
			&std::process::Command::new("podman")
				.args([
					"inspect",
					&format!("{proj}-web-1"),
					"--format",
					&format!("{{{{index .Config.Labels \"{key}\"}}}}"),
				])
				.output()
				.expect("podman inspect")
				.stdout,
		)
		.trim()
		.to_string()
	};
	let role = inspect("com.example.role");
	let env = inspect("com.example.env");
	engine.down(&file).await.unwrap();

	assert_eq!(
		role, "web",
		"the first label_file entry did not reach the container"
	);
	assert_eq!(
		env, "test",
		"the second label_file entry did not reach the container"
	);
}

#[tokio::test]
async fn label_file_with_control_char_in_value_is_rejected() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(dir.path().join("svc.labels"), b"com.example.team=bl\0ue\n").unwrap();
	let proj = proj("lfrej");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    label_file: svc.labels\n",
	)
	.unwrap();

	let err = engine.up(&file).await.unwrap_err();
	let msg = err.to_string();
	assert!(
		msg.contains("svc.labels") && msg.contains("line 1"),
		"rejection did not name the file and line: {msg}",
	);
}

#[tokio::test]
async fn label_file_with_thousand_lines_is_capped() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let mut content = String::new();
	for i in 0..1000 {
		content.push_str(&format!("com.example.k{i}=v\n"));
	}
	fs::write(dir.path().join("svc.labels"), content).unwrap();
	let proj = proj("lfcap");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    label_file: svc.labels\n",
	)
	.unwrap();

	let err = engine.up(&file).await.unwrap_err();
	assert!(
		err.to_string().contains("TooManyEntries"),
		"expected TooManyEntries, got: {err}",
	);
}

#[tokio::test]
async fn config_files_label_url_encodes_comma_in_path() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let composed = dir.path().join("a,b.yaml");
	fs::write(
		&composed,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let proj = proj("cfenc");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf())
		.with_compose_files(vec![composed.clone()]);
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	let label = String::from_utf8_lossy(
		&std::process::Command::new("podman")
			.args([
				"inspect",
				&format!("{proj}-web-1"),
				"--format",
				"{{index .Config.Labels \"podup.config-files\"}}",
			])
			.output()
			.expect("podman inspect")
			.stdout,
	)
	.trim()
	.to_string();
	engine.down(&file).await.unwrap();

	assert!(
		label.contains("%2C"),
		"the `,` in the path was not URL-encoded in the podup.config-files label: {label:?}",
	);
	assert!(
		!label.contains(','),
		"the unencoded `,` is still in the label, so a split on `,` would visually merge: {label:?}",
	);
}
