//! `push` against a registry that really receives the image.
//!
//! The other `push` tests pin the output shape and the unreachable-registry
//! failure. Neither executes the path where a push succeeds, so the command that
//! #598 catalogued as exiting 0 while failing had its success path asserted only
//! against a fake responder. The registry here is a container on the same
//! rootless Podman the rest of this suite already drives.
//!
//! **The assertion reads the image back out of the registry.** A zero exit is
//! what `push` used to return while writing nothing at all, so it cannot be the
//! evidence.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use tempfile::tempdir;

use super::*;

/// A free loopback port, chosen by binding zero and releasing it.
///
/// There is a window between releasing and the registry binding it. It is small
/// and the readiness poll below fails loudly rather than silently if it is lost,
/// which is the honest trade against hard-coding a port two concurrent runs
/// would fight over.
fn free_port() -> u16 {
	let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("no loopback port");
	listener.local_addr().unwrap().port()
}

/// One HTTP GET over a plain TCP socket, returning the body.
///
/// Raw rather than a client library on purpose: this asks a local registry for a
/// small JSON document, and pulling async HTTP machinery into a test buys
/// nothing but failure modes to debug. `None` means the request did not
/// complete, which the caller treats as "not ready yet".
fn http_get(port: u16, path: &str) -> Option<String> {
	let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
	stream
		.set_read_timeout(Some(std::time::Duration::from_secs(5)))
		.ok()?;
	write!(
		stream,
		"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
	)
	.ok()?;
	let mut response = String::new();
	stream.read_to_string(&mut response).ok()?;
	Some(response)
}

/// Wait for the registry to answer its version endpoint.
///
/// Polls with a deadline rather than sleeping a fixed amount: a sleep long
/// enough to be safe is wasted on every run, and one short enough to be quick is
/// a flake waiting for a slow host.
fn wait_until_ready(port: u16) -> bool {
	let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
	while std::time::Instant::now() < deadline {
		if let Some(response) = http_get(port, "/v2/") {
			if response.starts_with("HTTP/1.1 200") {
				return true;
			}
		}
		std::thread::sleep(std::time::Duration::from_millis(200));
	}
	false
}

/// Remove the registry container and the image built for it, whatever happened.
fn cleanup(container: &str, image: &str) {
	let _ = Command::new("podman")
		.args(["rm", "-f", container])
		.output();
	let _ = Command::new("podman").args(["rmi", "-f", image]).output();
}

#[tokio::test]
async fn cli_push_reaches_a_real_registry() {
	if super::podman().await.is_none() {
		return;
	}
	let port = free_port();
	let container = format!("t{}-podup-registry", std::process::id());
	let repository = "podup-push-check";
	let image = format!("127.0.0.1:{port}/{repository}:1");

	// The registry itself is started with podman rather than podup: the subject
	// of this test is `podup push`, and standing the fixture up with the same
	// binary would let one bug hide another.
	let start = Command::new("podman")
		.args([
			"run",
			"-d",
			"--name",
			&container,
			"-p",
			&format!("127.0.0.1:{port}:5000"),
			"docker.io/library/registry:2",
		])
		.output()
		.unwrap();
	if !start.status.success() {
		cleanup(&container, &image);
		panic!(
			"could not start the registry: {}",
			String::from_utf8_lossy(&start.stderr)
		);
	}
	if !wait_until_ready(port) {
		cleanup(&container, &image);
		panic!("the registry never answered /v2/ on port {port}");
	}

	let dir = tempdir().unwrap();
	let compose = dir.path().join("docker-compose.yml");
	fs::write(
		&compose,
		format!(
			"services:\n  app:\n    image: {image}\n    build:\n      context: .\n      \
			 dockerfile_inline: |\n        FROM docker.io/library/busybox:1.36\n        \
			 RUN echo pushed > /pushed\n"
		),
	)
	.unwrap();
	let proj = format!("t{}-pushreal", std::process::id());

	let build = Command::new(bin())
		.args(["-f", compose.to_str().unwrap(), "-p", &proj, "build"])
		.output()
		.unwrap();
	if !build.status.success() {
		cleanup(&container, &image);
		panic!("build failed: {}", String::from_utf8_lossy(&build.stderr));
	}

	let push = Command::new(bin())
		.args([
			"-f",
			compose.to_str().unwrap(),
			"-p",
			&proj,
			"push",
			"--tls-verify",
			"false",
		])
		.output()
		.unwrap();
	let push_ok = push.status.success();
	let push_err = String::from_utf8_lossy(&push.stderr).to_string();

	// Read the image back out of the registry. This is the assertion; the exit
	// code above is only reported alongside it, because exiting 0 while writing
	// nothing is the exact defect this test exists for.
	let catalog = http_get(port, "/v2/_catalog").unwrap_or_default();
	let tags = http_get(port, &format!("/v2/{repository}/tags/list")).unwrap_or_default();
	cleanup(&container, &image);

	assert!(push_ok, "push exited non-zero: {push_err}");
	assert!(
		catalog.contains(repository),
		"the registry does not list the repository after a successful push.\n\
		 catalog: {catalog}\npush stderr: {push_err}"
	);
	assert!(
		tags.contains("\"1\""),
		"the registry has the repository but not the tag that was pushed.\ntags: {tags}"
	);
}
