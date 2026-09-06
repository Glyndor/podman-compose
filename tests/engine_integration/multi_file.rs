//! Merge semantics across several `-f` files, measured against docker compose.
//!
//! #1078 landed the merge rules and was closed while three of its cases stayed
//! unverified: same-target `volumes` dedup, `!override` and `!reset`. All three
//! turn out to be implemented, measured here against docker compose v5.1.3 on
//! the same inputs, but nothing pinned them, so a regression would have been
//! silent. That is what these are for.
//!
//! They drive the CLI because multi-`-f` merging and project-directory anchoring
//! only exist there; `parse_str` takes a single document.
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

use super::*;

/// Write `contents` to `dir/name`, creating parent directories.
fn write(dir: &Path, name: &str, contents: &str) {
	let path = dir.join(name);
	if let Some(parent) = path.parent() {
		fs::create_dir_all(parent).unwrap();
	}
	fs::write(path, contents).unwrap();
}

/// `podup -f <files…> -p <proj> <args…>`.
fn podup(dir: &Path, files: &[&str], proj: &str, args: &[&str]) -> std::process::Output {
	let mut cmd = Command::new(bin());
	for f in files {
		cmd.arg("-f").arg(dir.join(f));
	}
	cmd.args(["-p", proj]).args(args);
	cmd.output().expect("run podup")
}

/// The rendered `config` for a multi-file project.
fn config(dir: &Path, files: &[&str], proj: &str) -> String {
	let out = podup(dir, files, proj, &["config"]);
	assert!(
		out.status.success(),
		"config failed: {}",
		String::from_utf8_lossy(&out.stderr)
	);
	String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn same_target_volume_is_replaced_not_duplicated() {
	// docker compose v5.1.3 on these exact files keeps only `volb:/data`. Appending
	// instead would hand Podman two mounts on one mount point.
	let dir = tempdir().unwrap();
	write(
		dir.path(),
		"a.yml",
		"services:\n  web:\n    image: alpine:latest\n    volumes:\n      - vola:/data\nvolumes:\n  vola:\n  volb:\n",
	);
	write(
		dir.path(),
		"b.yml",
		"services:\n  web:\n    volumes:\n      - volb:/data\n",
	);

	let rendered = config(dir.path(), &["a.yml", "b.yml"], &proj("mfvol"));
	assert!(
		rendered.contains("volb:/data"),
		"the overriding volume is missing: {rendered}"
	);
	assert!(
		!rendered.contains("vola:/data"),
		"both volumes survived on the same target: {rendered}"
	);
}

#[test]
fn volumes_on_different_targets_are_both_kept() {
	// The other half of the same rule, and the one that tells a real merge from a
	// wholesale replacement of the list, which would pass the test above too.
	let dir = tempdir().unwrap();
	write(
		dir.path(),
		"a.yml",
		"services:\n  web:\n    image: alpine:latest\n    volumes:\n      - vola:/data\nvolumes:\n  vola:\n  volb:\n",
	);
	write(
		dir.path(),
		"b.yml",
		"services:\n  web:\n    volumes:\n      - volb:/other\n",
	);

	let rendered = config(dir.path(), &["a.yml", "b.yml"], &proj("mfvol2"));
	assert!(
		rendered.contains("vola:/data") && rendered.contains("volb:/other"),
		"a distinct target was dropped, so the list was replaced rather than merged: {rendered}"
	);
}

#[test]
fn override_tag_replaces_the_whole_mapping() {
	// `!override` discards the base's keys instead of merging them.
	let dir = tempdir().unwrap();
	write(
		dir.path(),
		"a.yml",
		"services:\n  web:\n    image: alpine:latest\n    environment:\n      A: from-a\n      B: from-a\n",
	);
	write(
		dir.path(),
		"b.yml",
		"services:\n  web:\n    environment: !override\n      C: from-b\n",
	);

	let rendered = config(dir.path(), &["a.yml", "b.yml"], &proj("mfovr"));
	assert!(
		rendered.contains("from-b"),
		"the overriding value is missing: {rendered}"
	);
	assert!(
		!rendered.contains("from-a"),
		"!override merged instead of replacing: {rendered}"
	);
}

#[test]
fn reset_tag_removes_the_key() {
	// `!reset null` drops the base's value entirely rather than leaving it or
	// rendering an empty one.
	let dir = tempdir().unwrap();
	write(
		dir.path(),
		"a.yml",
		"services:\n  web:\n    image: alpine:latest\n    environment:\n      A: from-a\n",
	);
	write(
		dir.path(),
		"b.yml",
		"services:\n  web:\n    environment: !reset null\n",
	);

	let rendered = config(dir.path(), &["a.yml", "b.yml"], &proj("mfrst"));
	assert!(
		!rendered.contains("from-a"),
		"!reset left the base value in place: {rendered}"
	);
	assert!(
		!rendered.contains("environment:"),
		"!reset left an empty environment behind: {rendered}"
	);
}

#[tokio::test]
async fn relative_paths_anchor_to_the_first_file_not_each_file() {
	if super::podman().await.is_none() {
		return;
	}
	// The compose spec anchors a relative path to the *project* directory, the
	// first `-f`'s directory, not to the file the key was written in. Both
	// directories hold a `shared.env`, so reading the wrong one is visible rather
	// than merely absent, and docker compose v5.1.3 resolves this pair to the
	// root's file.
	//
	// `config` cannot answer this: podup renders `env_file` unresolved rather than
	// materialising it into `environment` the way docker does. So this asks the
	// running container, which is the behaviour that matters anyway.
	let dir = tempdir().unwrap();
	write(dir.path(), "shared.env", "WHICH=root-dir\n");
	write(dir.path(), "sub/shared.env", "WHICH=sub-dir\n");
	write(
		dir.path(),
		"a.yml",
		"services:\n  web:\n    image: alpine:latest\n    command: [\"sleep\", \"infinity\"]\n",
	);
	write(
		dir.path(),
		"sub/b.yml",
		"services:\n  web:\n    env_file:\n      - shared.env\n",
	);

	let files = ["a.yml", "sub/b.yml"];
	let project = proj("mfanch");
	podup(dir.path(), &files, &project, &["up", "-d"]);
	let anchored = podup(
		dir.path(),
		&files,
		&project,
		&[
			"exec",
			"-T",
			"web",
			"sh",
			"-c",
			"test \"$WHICH\" = root-dir",
		],
	)
	.status
	.success();
	podup(dir.path(), &files, &project, &["down", "-v"]);

	assert!(
		anchored,
		"a relative env_file declared in sub/b.yml resolved against its own directory \
		 instead of the project directory"
	);
}
