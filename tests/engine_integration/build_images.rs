//! Engine integration tests for build, networks, long-form secret/config refs
//! and the orphan sweep.
//!
//! Split out of `build_resources.rs` when that file passed the 500 code-line
//! hard limit.
use super::*;

// Build
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_with_target_stage() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	// Multi-stage Dockerfile — build with target: base covers build.rs L77.
	// Each stage leaves its name on disk, so which one was built is readable from
	// inside the container. With `RUN echo` alone both stages produce an image
	// that looks the same from outside, and a build that ignored `target:`
	// entirely would have passed.
	fs::write(
		dir.path().join("Dockerfile"),
		b"FROM alpine:latest AS base\nRUN echo base-stage > /stage\nFROM base AS final\nRUN echo final-stage > /stage\n",
	)
	.unwrap();

	let proj = proj("bst");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let image_tag = format!("podup-test-bst-{}:latest", std::process::id());
	let yaml = format!(
		"services:\n  app:\n    build:\n      context: .\n      target: base\n    image: {image_tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	let stage = engine
		.test_exec_capture(
			&format!("{proj}-app-1"),
			vec!["cat".into(), "/stage".into()],
		)
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", &image_tag])
		.status();

	assert_eq!(
		stage.trim(),
		"base-stage",
		"build stopped at the wrong stage: target: base must not run the final stage"
	);
}

#[tokio::test]
async fn build_with_args_and_extra_tags() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("bat");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let pid = std::process::id();
	let main_tag = format!("podup-test-bat-{}:latest", pid);
	let extra_tag = format!("podup-test-bat-extra-{}:v1", pid);
	let yaml = format!(
		"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        ARG VERSION=0\n        RUN echo Version $VERSION\n      args:\n        VERSION: \"1.0\"\n      tags:\n        - {extra_tag}\n    image: {main_tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	engine.down(&file).await.unwrap();
}

#[tokio::test]
async fn build_with_cli_no_cache_and_build_arg() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("bco");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let tag = format!("podup-test-bco-{}:latest", std::process::id());
	let yaml = format!(
		"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        ARG VERSION=0\n        RUN echo Version $VERSION\n      args:\n        VERSION: \"1.0\"\n    image: {tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	// CLI overrides: force no-cache and override the compose VERSION build arg.
	engine
		.build_all_with_options(
			&file,
			&[],
			&podup::BuildOptions {
				no_cache: true,
				build_args: vec!["VERSION=2.0".to_string()],
				..Default::default()
			},
		)
		.await
		.unwrap();
}

#[tokio::test]
async fn build_inline_dockerfile() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("bld");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let image_tag = format!("podup-test-build-{}:latest", std::process::id());
	let yaml = format!(
		"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        RUN echo built\n    image: {image_tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	engine.down(&file).await.unwrap();
}

#[tokio::test]
async fn build_from_dockerfile_in_context() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	fs::write(
		dir.path().join("Dockerfile"),
		b"FROM alpine:latest\nRUN echo context-build\n",
	)
	.unwrap();

	let proj = proj("bdc");
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let image_tag = format!("podup-test-build-ctx-{}:latest", std::process::id());
	let yaml = format!(
		"services:\n  app:\n    build:\n      context: .\n    image: {image_tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
// Networks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn explicit_network_created() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("net");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    networks:\n      - mynet\nnetworks:\n  mynet:\n    driver: bridge\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	engine.down(&file).await.unwrap();
}

// ---------------------------------------------------------------------------
// Secret/config long-form refs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn secret_long_form_ref() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("slf");
	let engine = Engine::new(client, proj.clone());
	// mode is octal notation per the Compose Specification (leading-zero `0400`);
	// uid is passed through to the native secret spec.
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    secrets:\n      - source: mysecret\n        target: /run/secrets/custom_name\n        mode: 0400\n        uid: \"0\"\nsecrets:\n  mysecret:\n    content: \"topsecret\"\n",
	)
	.unwrap();

	engine.up(&file).await.unwrap();
	// `cat` alone only proves the path exists — it exits 0 on an empty or wrong
	// file, and says nothing about the `mode:`/`uid:` the compose file asks for.
	// Check the content and the permissions the long form actually requested.
	let read = engine
		.exec_with_options(
			&file,
			"web",
			vec![
				"sh".to_string(),
				"-c".to_string(),
				"test \"$(cat /run/secrets/custom_name)\" = topsecret \
				 && test \"$(stat -c '%a %u' /run/secrets/custom_name)\" = '400 0'"
					.to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await;
	engine.down(&file).await.unwrap();
	assert!(
		read.is_ok(),
		"the long-form secret did not land at the requested target with mode 0400 and uid 0: {read:?}"
	);
}

#[tokio::test]
async fn config_long_form_ref() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("clf");
	let engine = Engine::new(client, proj.clone());
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    configs:\n      - source: mycfg\n        target: /etc/app.conf\nconfigs:\n  mycfg:\n    content: \"key=value\"\n",
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
				"test \"$(cat /etc/app.conf)\" = key=value".to_string(),
			],
			podup::ExecOptions::default(),
		)
		.await;
	engine.down(&file).await.unwrap();
	assert!(
		read.is_ok(),
		"the long-form config did not land at /etc/app.conf with its content: {read:?}"
	);
}

// ---------------------------------------------------------------------------
// External volume skip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn external_volume_missing_errors_on_up() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("exv");
	let engine = Engine::new(client, proj.clone());
	// An external volume that does not exist must surface an error rather than
	// being silently skipped (compose spec requires the resource to exist).
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\nvolumes:\n  extdata-does-not-exist:\n    external: true\n",
	)
	.unwrap();

	let result = engine.up(&file).await;
	assert!(
		matches!(result, Err(podup::ComposeError::ExternalNotFound(_))),
		"expected ExternalNotFound, got {result:?}"
	);
}

// ---------------------------------------------------------------------------
// External (Podman-native) secret injection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn external_secret_missing_errors_on_up() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("exsec");
	let engine = Engine::new(client, proj.clone());
	// An `external: true` secret that no `podman secret` backs must fail closed,
	// like an external volume, rather than start a container missing the secret.
	let file = parse_str(
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    secrets: [absent-secret]\nsecrets:\n  absent-secret:\n    external: true\n",
	)
	.unwrap();

	let result = engine.up(&file).await;
	assert!(
		matches!(result, Err(podup::ComposeError::ExternalNotFound(_))),
		"expected ExternalNotFound, got {result:?}"
	);
}

#[cfg(feature = "test-helpers")]
#[tokio::test]
async fn external_secret_injected_into_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("insec");
	let secret_name = format!("{proj}-tok");

	// Create the backing Podman secret out-of-band — the external-secret idiom.
	// Skip the test if the podman CLI is unavailable (socket alone is not enough).
	let dir = tempfile::tempdir().unwrap();
	let secret_src = dir.path().join("tok");
	fs::write(&secret_src, b"native-secret-value").unwrap();
	let created = std::process::Command::new("podman")
		.args([
			"secret",
			"create",
			&secret_name,
			secret_src.to_str().unwrap(),
		])
		.status();
	match created {
		Ok(s) if s.success() => {}
		_ => return,
	}

	// The compose name is `tok` (→ /run/secrets/tok); the actual secret is named
	// differently, exercising the source/target split.
	let yaml = format!(
		"services:\n  app:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    secrets: [tok]\nsecrets:\n  tok:\n    external: true\n    name: {secret_name}\n"
	);
	let file = parse_str(&yaml).unwrap();
	let engine = Engine::new(client, proj.clone());
	engine.up(&file).await.unwrap();
	let cname = format!("{proj}-app-1");
	let out = engine
		.test_exec_capture(&cname, vec!["cat".into(), "/run/secrets/tok".into()])
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let _ = std::process::Command::new("podman")
		.args(["secret", "rm", &secret_name])
		.status();

	assert!(
		out.contains("native-secret-value"),
		"external secret was not injected at /run/secrets/tok: {out:?}"
	);
}

// ---------------------------------------------------------------------------
// Orphan removal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remove_orphans_removes_container() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let proj = proj("orr");
	let engine = Engine::new(client, proj.clone());

	let file_svc1 = parse_str(
		"services:\n  svc1:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	engine.up(&file_svc1).await.unwrap();

	// file_svc2 only declares svc2 — svc1 becomes an orphan
	let file_svc2 = parse_str(
		"services:\n  svc2:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	engine.remove_orphans(&file_svc2).await.unwrap();
	let survivors = engine
		.test_project_container_names()
		.await
		.unwrap_or_default();

	// cleanup (svc1 already removed; down() on either file is a no-op for missing containers)
	let _ = engine.down(&file_svc1).await;

	// The sibling test in lifecycle.rs pins that a sweep with no orphan present
	// removes nothing. This one is the other direction, and until now neither
	// looked: a sweep that removed nothing at all satisfied both.
	assert!(
		!survivors.iter().any(|n| n.contains("-svc1-")),
		"the orphaned svc1 container survived the sweep: {survivors:?}"
	);
}

// ---------------------------------------------------------------------------

/// The image ID a tag currently resolves to, via the podman CLI — the same
/// out-of-band check the sibling tests in this file use to observe state podup
/// itself reports on. Empty when the tag is absent.
fn image_id(tag: &str) -> String {
	let out = std::process::Command::new("podman")
		.args(["inspect", tag, "--format", "{{.Id}}"])
		.output()
		.expect("run podman inspect");
	String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// #1094: `up` must not rebuild a service whose image is already present.
///
/// It used to rebuild unconditionally, *with* the cache, so the rebuild could
/// resolve to an older layer chain and retag the image backwards — silently
/// discarding a `build --no-cache` that had just run. That breaks the ordinary
/// deploy shape: build explicitly, then start.
///
/// The sequence matters. A plain `build` first seeds the cache with one chain;
/// `build --no-cache` then produces a different one and tags it. Without both
/// steps there is no older chain for `up` to revert to and the bug does not
/// appear at all.
#[tokio::test]
async fn up_keeps_the_image_a_no_cache_build_produced() {
	let client = match podman().await {
		Some(d) => d,
		None => return,
	};
	let dir = tempfile::tempdir().unwrap();
	let proj = proj("upnb");
	let tag = format!("podup-test-upnb-{}:latest", std::process::id());
	let engine = Engine::with_base_dir(client, proj.clone(), dir.path().to_path_buf());
	let yaml = format!(
		"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        RUN echo marker > /marker\n    image: {tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	// Seed the cache with one chain, then force a different one.
	engine.build_all(&file, &[]).await.unwrap();
	engine
		.build_all_with_options(
			&file,
			&[],
			&podup::BuildOptions {
				no_cache: true,
				..Default::default()
			},
		)
		.await
		.unwrap();
	let after_build = image_id(&tag);

	engine
		.up_with_options(&file, true, &[], &[], false, false, false)
		.await
		.unwrap();
	let after_up = image_id(&tag);

	assert_eq!(
		after_build, after_up,
		"`up` must not retag the image built by `build --no-cache`"
	);

	engine.down_with_options(&file, true).await.ok();
}
