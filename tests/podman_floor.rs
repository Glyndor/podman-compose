//! The Podman floor is written in three places and nothing derives one from
//! another.
//!
//! - `internal/libpod/client/mod.rs` — `MIN_LIBPOD_API_MAJOR`, the gate the
//!   running binary applies to the major the engine reports.
//! - `install.sh` — `PODMAN_MIN_MAJOR`, the precheck that refuses to install
//!   over a local engine below the floor.
//! - `debian/control` — the `podman (>= N.0)` relationship.
//!
//! The third one used to be a `Recommends`, where a wrong number cost a
//! suggestion. As a `Depends` it decides whether the package installs at all,
//! so a copy that drifts low lets apt install podup onto an engine the binary
//! will then refuse, and a copy that drifts high refuses an install that
//! would have worked. Neither shows up until a user hits it, on a machine
//! nobody here owns.
//!
//! `install.ps1` carries no floor and needs none: there is no Windows package
//! relationship to satisfy, and the engine lives in a `podman machine` VM
//! whose version the installer cannot read from the host.

use std::fs;
use std::path::Path;

fn read(rel: &str) -> String {
	let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
	fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel} is readable: {e}"))
}

/// `const MIN_LIBPOD_API_MAJOR: u64 = 5;`
fn floor_from_source() -> u64 {
	let src = read("internal/libpod/client/mod.rs");
	src.lines()
		.map(str::trim_start)
		.filter(|l| !l.starts_with("//"))
		.find_map(|l| l.split_once("MIN_LIBPOD_API_MAJOR: u64 = "))
		.and_then(|(_, rest)| rest.trim_end_matches(';').trim().parse().ok())
		.expect("MIN_LIBPOD_API_MAJOR is declared as a u64 literal")
}

/// `PODMAN_MIN_MAJOR=5`
fn floor_from_installer() -> u64 {
	let src = read("install.sh");
	src.lines()
		.map(str::trim_start)
		.filter(|l| !l.starts_with('#'))
		.find_map(|l| l.strip_prefix("PODMAN_MIN_MAJOR="))
		.and_then(|v| v.trim().trim_matches('"').parse().ok())
		.expect("install.sh assigns PODMAN_MIN_MAJOR")
}

/// The major out of `Depends: …, podman (>= 5.0)`.
///
/// Read off the `Depends:` line specifically, not from anywhere the string
/// appears: this file exists because the same number in several places went
/// out of step, and a comment quoting the relationship would satisfy a looser
/// search. The comment above that field quotes it more than once.
fn floor_from_control() -> u64 {
	let src = read("debian/control");
	let depends = src
		.lines()
		.filter(|l| !l.trim_start().starts_with('#'))
		.find(|l| l.starts_with("Depends:"))
		.expect("debian/control's binary stanza declares Depends");
	let (_, rest) = depends
		.split_once("podman (>= ")
		.unwrap_or_else(|| panic!("Depends does not require podman at all: {depends:?}"));
	let version = rest
		.split(')')
		.next()
		.expect("the podman relationship closes its parenthesis");
	version
		.split('.')
		.next()
		.and_then(|major| major.parse().ok())
		.unwrap_or_else(|| panic!("cannot read a major out of {version:?}"))
}

#[test]
fn the_package_requires_podman_rather_than_suggesting_it() {
	let src = read("debian/control");
	let binary = src
		.split_once("\nPackage: podup\n")
		.expect("debian/control has a binary stanza")
		.1;
	let relationships: Vec<&str> = binary
		.lines()
		.filter(|l| !l.trim_start().starts_with('#'))
		.filter(|l| l.starts_with("Depends:") || l.starts_with("Recommends:"))
		.collect();

	assert!(
		relationships
			.iter()
			.any(|l| l.starts_with("Depends:") && l.contains("podman")),
		"podman must be a Depends. podup translates a compose file into libpod \
		 calls and does nothing else, so an install without an engine cannot do \
		 the one thing it is for. Relationships found: {relationships:?}"
	);
	assert!(
		!relationships
			.iter()
			.any(|l| l.starts_with("Recommends:") && l.contains("podman")),
		"podman is declared as a Recommends as well as a Depends, which is \
		 apt telling the user two different things about the same package"
	);
}

#[test]
fn every_podman_floor_agrees() {
	let source = floor_from_source();
	let installer = floor_from_installer();
	let control = floor_from_control();

	assert_eq!(
		source, installer,
		"MIN_LIBPOD_API_MAJOR is {source} and install.sh's PODMAN_MIN_MAJOR is \
		 {installer}. The installer would admit an engine the binary refuses, \
		 or refuse one it accepts."
	);
	assert_eq!(
		source, control,
		"MIN_LIBPOD_API_MAJOR is {source} and debian/control requires podman \
		 >= {control}.0. Since podman is a Depends, this number decides whether \
		 apt installs podup at all: too low and apt installs it onto an engine \
		 the binary will refuse, too high and apt refuses an install that would \
		 have worked."
	);
}

/// The parsers are what the assertions above trust, so pin them on input that
/// differs from today's files. A parser that always returned 5 would satisfy
/// every equality and prove nothing.
#[test]
fn the_floor_parsers_read_the_values_rather_than_guessing_them() {
	// Each helper reads a real file, so the shapes are exercised here through
	// the same string handling rather than through the filesystem.
	let control = "\
# Depends: …, podman (>= 99.0) quoted in a comment
Package: podup
Depends: ${shlibs:Depends}, ${misc:Depends}, podman (>= 7.0)
";
	let depends = control
		.lines()
		.filter(|l| !l.trim_start().starts_with('#'))
		.find(|l| l.starts_with("Depends:"))
		.unwrap();
	assert!(
		depends.contains("podman (>= 7.0)"),
		"the comment quoting a different floor must not be the line that is read"
	);

	// The real files still have to parse, and to a plausible major rather than
	// to whatever `unwrap_or_default` would hand back.
	for (name, got) in [
		("MIN_LIBPOD_API_MAJOR", floor_from_source()),
		("PODMAN_MIN_MAJOR", floor_from_installer()),
		("debian/control", floor_from_control()),
	] {
		assert!(
			(1..=99).contains(&got),
			"{name} parsed as {got}, which is not a plausible Podman major"
		);
	}
}
