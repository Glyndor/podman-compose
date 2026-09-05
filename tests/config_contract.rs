//! Contract tests for `podup config`: the rendered network names must match
//! what `up` actually creates, so a reader of `config` cannot be misled by a
//! network the next `up` would silently rename.
//!
//! #1698: with a one-service file that declares no networks, `config`
//! emitted `default: null` while `up` created `<project>_default`. Both views
//! now agree: `config` prints the resolved name on every declared network,
//! calling the same `Engine::resolve_network_name` `up` calls rather than
//! duplicating the rule.
//!
//! `config --hash` and `--quiet` are unchanged: a regression that re-introduces
//! the bug should be caught here, and the existing render-time unit tests
//! cover the hash and quiet paths.

mod harness;

use harness::{bin, podman_up};
use std::process::Command;
use tempfile::tempdir;

/// The network name `config` prints must be a network `podman network ls`
/// reports under the same project. With no explicit network declared, `config`
/// resolved to `<project>_default` and `up` created exactly that, so the two
/// views must agree; otherwise the operator reading `config` would see one
/// name and the next `up` would silently create another.
#[tokio::test]
async fn config_default_network_name_is_what_up_creates() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	std::fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();
	let proj = format!("t{}-cfgnet", std::process::id());

	let cfg = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "config"])
		.output()
		.expect("run podup config");
	assert!(
		cfg.status.success(),
		"config must succeed: {}",
		String::from_utf8_lossy(&cfg.stderr)
	);
	let cfg_text = String::from_utf8_lossy(&cfg.stdout);
	// The bug was `default: null`; the fix is `default:` followed by the
	// resolved name.
	assert!(
		!cfg_text.contains("default: null"),
		"`config` must not leave `default: null`; got:\n{cfg_text}"
	);
	let expected = format!("name: {proj}_default");
	assert!(
		cfg_text.contains(&expected),
		"`config` must print `{expected}` for the implicit `default` network; \
		 got:\n{cfg_text}"
	);

	let up = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "up", "-d"])
		.output()
		.expect("run podup up -d");
	assert!(
		up.status.success(),
		"up -d must succeed: {}",
		String::from_utf8_lossy(&up.stderr)
	);

	let ls = Command::new("podman")
		.args(["network", "ls", "--format", "{{.Name}}"])
		.output()
		.expect("run podman network ls");
	assert!(
		ls.status.success(),
		"podman network ls must succeed: {}",
		String::from_utf8_lossy(&ls.stderr)
	);
	let ls_text = String::from_utf8_lossy(&ls.stdout);
	let expected_name = format!("{proj}_default");
	assert!(
		ls_text.lines().any(|l| l.trim() == expected_name),
		"`{expected_name}` must appear in `podman network ls`:\n{ls_text}"
	);

	let _ = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "down", "-v"])
		.output();
}

/// A network declared without a `name:` is also stamped with the project
/// prefix. The fixture declares one such network alongside an explicit one,
/// and `config` must show both. The implicit `default` network shows up too,
/// because the service has no explicit `networks:` block.
#[tokio::test]
async fn config_names_both_implicit_and_explicit_networks() {
	if !podman_up().await {
		return;
	}
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	std::fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\nnetworks:\n  backend:\n    name: my-custom-net\n  monitoring:\n",
	)
	.unwrap();
	let proj = format!("t{}-cfgboth", std::process::id());
	let cfg = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "config"])
		.output()
		.expect("run podup config");
	assert!(
		cfg.status.success(),
		"`config` must succeed: {}",
		String::from_utf8_lossy(&cfg.stderr)
	);
	let text = String::from_utf8_lossy(&cfg.stdout);
	assert!(
		!text.contains(": null\n"),
		"`config` must not leave any network with `null` body: {text}"
	);
	let expected_default = format!("name: {proj}_default");
	assert!(
		text.contains(&expected_default),
		"`config` must print `{expected_default}`: {text}"
	);
	assert!(
		text.contains("name: my-custom-net"),
		"`config` must keep the explicit `name:`: {text}"
	);
	let expected_monitoring = format!("name: {proj}_monitoring");
	assert!(
		text.contains(&expected_monitoring),
		"`config` must print `{expected_monitoring}` for the bare `monitoring` network: {text}"
	);
}

/// `--hash` and `--quiet` remain unaffected by the network-name injection.
/// A regression that mutates the file before hashing or that prints under
/// `--quiet` would break the existing render-time unit tests; this contract
/// test pins the binary-level behaviour.
#[tokio::test]
async fn config_hash_and_quiet_are_unaffected() {
	let dir = tempdir().unwrap();
	let compose = dir.path().join("compose.yaml");
	std::fs::write(
		&compose,
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	)
	.unwrap();

	let hash_out = Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			"thashcfg",
			"config",
			"--hash",
			"web",
		])
		.output()
		.expect("run config --hash");
	assert!(hash_out.status.success(), "--hash must succeed");
	let hash = String::from_utf8_lossy(&hash_out.stdout);
	let mut tokens = hash.split_whitespace();
	let name = tokens.next().unwrap_or_default();
	let digest = tokens.next().unwrap_or_default();
	assert_eq!(
		name, "web",
		"`config --hash` first token is the service name: {hash:?}"
	);
	assert_eq!(
		digest.len(),
		64,
		"`config --hash` second token is a sha-256 hex digest: {hash:?}"
	);
	assert!(
		digest.chars().all(|c| c.is_ascii_hexdigit()),
		"`config --hash` second token is hex: {hash:?}"
	);

	let quiet = Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			"tquietcfg",
			"config",
			"--quiet",
		])
		.output()
		.expect("run config --quiet");
	assert!(
		quiet.status.success(),
		"`config --quiet` must succeed: {}",
		String::from_utf8_lossy(&quiet.stderr)
	);
	assert!(
		String::from_utf8_lossy(&quiet.stdout).trim().is_empty(),
		"`config --quiet` prints nothing on stdout"
	);
}
