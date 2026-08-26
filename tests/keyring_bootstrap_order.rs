//! `docs/debian-packaging.md` must check the archive key before installing it.
//!
//! The bootstrap this document publishes used to read `dpkg -i` first and
//! `gpg --show-keys /usr/share/keyrings/glyndor.gpg` second. `dpkg -i` runs the
//! package's maintainer scripts as root, so that order verified a keyring after
//! giving the package that wrote it full privileges — the check ran on its own
//! subject. `dpkg-deb -x` unpacks the data archive and runs nothing, which is
//! why the fingerprint has to come from an extracted copy.
//!
//! The same inversion was live in four places at once: this file, the apt
//! repository's README, the index page apt publishes, and the installer script
//! it generates. Fixing prose resets the clock; this test is what makes the
//! order a property rather than a habit.
//!
//! What it does NOT check, and deliberately: whether the fingerprint printed by
//! the documented command is the right one, and whether the anchor it links to
//! still exists in the apt repository. Both live in another repository, so this
//! test cannot see them change. apt's own tests/readme-bootstrap.test.sh covers
//! that side.

use std::path::Path;

const DOC: &str = "docs/debian-packaging.md";

fn doc() -> String {
	let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(DOC);
	std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Line index of the first line containing `needle`, or a failure naming what
/// was being looked for — an `Option` that silently becomes `None` would let a
/// renamed command turn this test green.
fn line_of(text: &str, needle: &str) -> usize {
	text.lines()
		.position(|l| l.contains(needle))
		.unwrap_or_else(|| panic!("{DOC} no longer contains {needle:?}"))
}

#[test]
fn the_keyring_is_unpacked_before_it_is_installed() {
	let text = doc();
	let extract = line_of(&text, "dpkg-deb -x glyndor-archive-keyring.deb");
	let install = line_of(&text, "sudo dpkg -i glyndor-archive-keyring.deb");
	assert!(
		extract < install,
		"{DOC} tells the reader to run `sudo dpkg -i` (line {}) before \
		 unpacking with `dpkg-deb -x` (line {}). `dpkg -i` runs maintainer \
		 scripts as root, so nothing from the package may run before its key \
		 has been checked.",
		install + 1,
		extract + 1,
	);
}

#[test]
fn the_fingerprint_is_read_before_the_install() {
	let text = doc();
	let show = line_of(&text, "gpg --show-keys");
	let install = line_of(&text, "sudo dpkg -i glyndor-archive-keyring.deb");
	assert!(
		show < install,
		"{DOC} reads the fingerprint at line {} but installs at line {}. A \
		 check that runs after the thing it gates proves nothing.",
		show + 1,
		install + 1,
	);
}

#[test]
fn the_fingerprint_comes_from_the_unpacked_copy() {
	let text = doc();
	let show = text
		.lines()
		.find(|l| l.contains("gpg --show-keys"))
		.expect("no `gpg --show-keys` line");
	assert!(
		show.contains("keyring-check/"),
		"{DOC} reads the fingerprint from {show:?}. Before the package is \
		 installed there is no /usr/share/keyrings copy to read, and after it \
		 is installed the file belongs to the package being checked; read the \
		 copy `dpkg-deb -x` unpacked instead.",
	);
}

#[test]
fn the_fingerprint_is_still_compared_against_another_host() {
	let text = doc();
	assert!(
		text.contains("github.com/Glyndor/apt#verify-the-signing-key"),
		"{DOC} no longer points at the apt README for the fingerprint. \
		 Comparing a key against the archive that served it is \
		 self-attestation; the second channel is the whole point.",
	);
}
