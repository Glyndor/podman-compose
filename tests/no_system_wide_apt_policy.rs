//! podup does not decide the machine's update policy.
//!
//! `unattended-upgrades` is a hard `Depends`, so podup can be sure the package
//! is installed. Whether it *runs* is a different file:
//! `/etc/apt/apt.conf.d/20auto-upgrades`, which switches automatic upgrades on
//! for every package on the machine rather than for Glyndor's. That is the
//! operator's call, or a fleet manager's. A container runtime is not the thing
//! that should make it, and #1593 settled it that way: leave the one uncovered
//! row (a Debian where somebody registered the archive by hand and ran
//! `apt install podup`) rather than close it from a `postinst`.
//!
//! The consequence is that the README owes the reader a sentence, because the
//! dependency looks like it delivers the outcome and does not. Both halves of
//! that live in prose, and prose stops being true without anything failing.
//! These are the gate.

use std::fs;
use std::path::Path;

fn read(rel: &str) -> String {
	let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
	fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// Every file dpkg would install or run, not only the maintainer scripts: a
/// conffile shipped under `debian/` reaches `/etc` just as effectively as a
/// `postinst` that writes one.
#[test]
fn nothing_podup_packages_writes_the_machine_wide_upgrade_switch() {
	let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("debian");
	let mut offenders = Vec::new();
	for entry in fs::read_dir(&dir).expect("debian/ is readable") {
		let path = entry.expect("debian/ entry is readable").path();
		if !path.is_file() {
			continue;
		}
		let name = path.file_name().unwrap().to_string_lossy().into_owned();
		// `debian/changelog` describes releases and may well mention the switch
		// by name when explaining that podup does not write it.
		if name == "changelog" || name == "copyright" {
			continue;
		}
		let Ok(text) = fs::read_to_string(&path) else {
			continue; // a binary artefact under debian/ writes no apt policy
		};
		// Comments are stripped, and finding that out is the reason this comment
		// exists: `debian/control` names the switch in the paragraph explaining
		// that podup does not write it, so matching raw text reported the
		// documentation of the rule as a breach of it. `#` opens a comment in
		// every format under `debian/` this test reads, control and rules and
		// shell alike. A line that emits the switch from a heredoc is still a
		// line, so it survives the strip and is still caught.
		let effective: String = text
			.lines()
			.filter(|l| !l.trim_start().starts_with('#'))
			.collect::<Vec<_>>()
			.join("\n");
		for needle in ["20auto-upgrades", "APT::Periodic", "Unattended-Upgrade::"] {
			if effective.contains(needle) {
				offenders.push(format!("debian/{name} contains {needle}"));
			}
		}
	}
	assert!(
		offenders.is_empty(),
		"podup would be setting update policy for every package on the machine, \
		 which #1593 decided it must not. The Glyndor archive's own allowlist \
		 belongs to glyndor-archive-keyring, not here. Found: {offenders:#?}"
	);
}

/// A tripwire rather than a rule. A maintainer script is not forbidden, but the
/// reason podup has none is the decision above, so adding the first one should
/// cost a deliberate look rather than passing as routine packaging.
#[test]
fn adding_a_maintainer_script_asks_you_to_re_read_the_decision() {
	let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("debian");
	let mut scripts = Vec::new();
	for entry in fs::read_dir(&dir).expect("debian/ is readable") {
		let name = entry
			.expect("debian/ entry is readable")
			.file_name()
			.to_string_lossy()
			.into_owned();
		// dpkg accepts both `podup.postinst` and a bare `postinst` in a
		// single-binary source package, so match the suffix rather than the name.
		if ["postinst", "preinst", "postrm", "prerm"]
			.iter()
			.any(|s| name == *s || name.ends_with(&format!(".{s}")))
		{
			scripts.push(name);
		}
	}
	assert!(
		scripts.is_empty(),
		"podup ships no maintainer scripts, and #1593 is why: the one thing a \
		 postinst was proposed for was writing /etc/apt/apt.conf.d/20auto-upgrades, \
		 which is the machine's policy and not podup's. If this script is for \
		 something else, say so and delete this test rather than widening it. \
		 Found: {scripts:#?}"
	);
}

/// Neither the line endings nor the wrap points are the claim, and both broke
/// this test before they were flattened away. The Windows checkout is CRLF, so a
/// needle spanning a line break cannot match there at all; and reflowing the
/// paragraph moves where the breaks fall, which would fail the test while the
/// README still said exactly the right thing. Collapsing every run of whitespace
/// to a single space leaves only the words, which are what is being asserted.
fn flattened(text: &str) -> String {
	text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The dependency reads like a promise that podup keeps itself up to date. On
/// one install path it does not, so the README has to say so; a user who never
/// runs `apt upgrade` is otherwise running whatever they installed months ago
/// while believing the opposite.
#[test]
fn the_readme_says_what_the_dependency_does_not_guarantee() {
	let readme = flattened(&read("README.md"));
	for needle in [
		"is installed, not that it is running",
		"20auto-upgrades",
		"apt install podup",
	] {
		assert!(
			readme.contains(needle),
			"README.md no longer tells the reader that depending on \
			 unattended-upgrades does not switch it on. Missing: {needle:?}"
		);
	}
}
