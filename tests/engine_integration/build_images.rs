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
		"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        ARG VERSION=0\n        RUN echo $$VERSION > /version\n      args:\n        VERSION: \"1.0\"\n      tags:\n        - {extra_tag}\n    image: {main_tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	// Two separate problems, both of which made the old version unfalsifiable.
	// `RUN echo` leaves nothing in the image, so a build that dropped the arg
	// produced a byte-identical result — hence writing the value to a file. And
	// the `$$` is load-bearing: compose substitutes `$VERSION` in the YAML before
	// the Dockerfile is ever assembled, so the single-dollar form the old test
	// used reached the build as an empty string and the ARG was never exercised
	// at all.
	let version = engine
		.test_exec_capture(
			&format!("{proj}-app-1"),
			vec!["cat".into(), "/version".into()],
		)
		.await
		.unwrap_or_default();
	let extra = image_id(&extra_tag);
	let main = image_id(&main_tag);
	engine.down(&file).await.unwrap();
	for t in [&main_tag, &extra_tag] {
		let _ = std::process::Command::new("podman")
			.args(["rmi", "-f", t])
			.status();
	}

	assert_eq!(
		version.trim(),
		"1.0",
		"the build arg did not reach the image"
	);
	assert!(!extra.is_empty(), "the extra tag was never applied");
	assert_eq!(
		extra, main,
		"the extra tag points at a different image than the main one"
	);
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
		"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        ARG VERSION=0\n        RUN echo $$VERSION > /version\n      args:\n        VERSION: \"1.0\"\n    image: {tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	// CLI overrides: force no-cache and override the compose VERSION build arg.
	engine
		.build_all_with_options(
			&file,
			&[],
			&podup::BuildOptions::new(true, false, vec!["VERSION=2.0".to_string()], false),
		)
		.await
		.unwrap();
	// The point of the override is that 2.0 wins over the 1.0 in the compose file.
	// This test never starts a container, so read the baked value straight out of
	// the image with a throwaway run.
	let baked = String::from_utf8_lossy(
		&std::process::Command::new("podman")
			.args(["run", "--rm", &tag, "cat", "/version"])
			.output()
			.expect("podman run")
			.stdout,
	)
	.trim()
	.to_string();
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", &tag])
		.status();

	assert_eq!(
		baked, "2.0",
		"the CLI build arg did not override the value in the compose file"
	);
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
		"services:\n  app:\n    build:\n      context: .\n      dockerfile_inline: |\n        FROM alpine:latest\n        RUN echo inline-built > /built\n    image: {image_tag}\n    command: [\"sleep\", \"infinity\"]\n"
	);
	let file = parse_str(&yaml).unwrap();

	engine.up(&file).await.unwrap();
	// `RUN echo built` left nothing to look at, so an inline Dockerfile that was
	// ignored in favour of a plain `FROM alpine` pull produced the same container.
	let built = engine
		.test_exec_capture(
			&format!("{proj}-app-1"),
			vec!["cat".into(), "/built".into()],
		)
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", &image_tag])
		.status();

	assert_eq!(
		built.trim(),
		"inline-built",
		"dockerfile_inline was not the image that got built"
	);
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
		b"FROM alpine:latest\nRUN echo context-build > /built\n",
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
	// The context's own Dockerfile has to be the one that built the image. With
	// `RUN echo` alone, a build that fell back to pulling alpine straight looked
	// identical from outside.
	let built = engine
		.test_exec_capture(
			&format!("{proj}-app-1"),
			vec!["cat".into(), "/built".into()],
		)
		.await
		.unwrap_or_default();
	engine.down(&file).await.unwrap();
	let _ = std::process::Command::new("podman")
		.args(["rmi", "-f", &image_tag])
		.status();

	assert_eq!(
		built.trim(),
		"context-build",
		"the Dockerfile in the build context was not used"
	);
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
	// These two are NOT mutation-proved, and the reason is worth keeping. Both
	// mutations I tried — never creating declared networks, and creating them
	// under a different name — take `up` itself down, because the container
	// references a network that is then missing. So the old `up().unwrap()` did
	// already cover "the network exists under the name the container expects".
	//
	// What these add is the contract in the open: the name carries the project
	// prefix, and the container is actually attached rather than silently left on
	// the default network. They would catch a future change where `up` stops
	// failing on a missing network. Until then they are documentation with teeth,
	// not verified coverage.
	let networks = String::from_utf8_lossy(
		&std::process::Command::new("podman")
			.args(["network", "ls", "--format", "{{.Name}}"])
			.output()
			.expect("podman network ls")
			.stdout,
	)
	.to_string();
	let attached = String::from_utf8_lossy(
		&std::process::Command::new("podman")
			.args([
				"inspect",
				&format!("{proj}-web-1"),
				"--format",
				"{{range $k, $v := .NetworkSettings.Networks}}{{$k}} {{end}}",
			])
			.output()
			.expect("podman inspect")
			.stdout,
	)
	.to_string();
	engine.down(&file).await.unwrap();

	let expected = format!("{proj}_mynet");
	assert!(
		networks.lines().any(|n| n == expected),
		"the declared network {expected} was not created: {networks:?}"
	);
	assert!(
		attached.contains(&expected),
		"the container was not attached to {expected}: {attached:?}"
	);
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
			&podup::BuildOptions::new(true, false, Vec::new(), false),
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
