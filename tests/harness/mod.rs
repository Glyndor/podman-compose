//! Shared fixtures for the two output-contract suites.
//!
//! Every integration test file is its own crate, so this is a module each of
//! them declares rather than something either imports from the other. Both
//! suites drive the real binary against a real Podman, which is the only layer
//! that can see a wrong exit code or an empty line (see the testing standard's
//! capability matrix).

#![allow(dead_code)]

use std::fs;
use std::process::Command;
use tempfile::tempdir;

pub fn bin() -> &'static str {
	env!("CARGO_BIN_EXE_podup")
}

/// Whether a Podman podup can actually drive is reachable.
///
/// This is the integration suite's guard, not a `podman info` probe. The CI
/// runner ships a podman binary that is *below podup's floor* with no socket
/// running, so `podman info` succeeds while every command here fails — the
/// weaker check let these run in the main CI job and fail for the environment
/// rather than the code.
pub async fn podman_up() -> bool {
	match podup::podman::connect_from_env().or_else(|_| podup::podman::connect(None)) {
		Ok(client) => client.ping().await.is_ok(),
		Err(_) => false,
	}
}

pub struct Project {
	pub _dir: tempfile::TempDir,
	pub compose: String,
	pub name: String,
}

impl Project {
	pub fn start(tag: &str) -> Self {
		let dir = tempdir().unwrap();
		let compose = dir.path().join("compose.yaml");
		fs::write(
			&compose,
			"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n    \
			 ports:\n      - \"0:80\"\n    volumes:\n      - data:/data\nvolumes:\n  data:\n",
		)
		.unwrap();
		let p = Project {
			compose: compose.to_string_lossy().into_owned(),
			name: format!("t{}-{tag}", std::process::id()),
			_dir: dir,
		};
		p.run(&["up", "-d"]);
		p
	}

	pub fn run(&self, args: &[&str]) -> String {
		let out = Command::new(bin())
			.args(["-f", &self.compose, "-p", &self.name])
			.args(args)
			.output()
			.expect("run podup");
		String::from_utf8_lossy(&out.stdout).into_owned()
	}

	/// The progress stream, which lifecycle commands write to stderr so stdout
	/// stays a clean pipe. [`Project::run`] returns stdout and therefore cannot
	/// see any of it.
	pub fn progress(&self, args: &[&str]) -> String {
		let out = Command::new(bin())
			.args(["-f", &self.compose, "-p", &self.name])
			.args(args)
			.output()
			.expect("run podup");
		String::from_utf8_lossy(&out.stderr).into_owned()
	}
}

impl Drop for Project {
	fn drop(&mut self) {
		let _ = Command::new(bin())
			.args(["-f", &self.compose, "-p", &self.name, "down", "-v"])
			.output();
	}
}
