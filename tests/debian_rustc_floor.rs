//! `debian/control` must declare the same rustc floor the crate does.
//!
//! The `Build-Depends: rustc (>= X)` line is what apt checks before building
//! the `.deb`, and it was written by hand from `rust-version` rather than
//! derived from it. Nothing kept the two in step.
//!
//! That matters because of *when* the mismatch surfaces. Raising
//! `rust-version` — which a dependency can force on an ordinary `cargo update`
//! — leaves `control` behind, and the `.deb` is only built during a release,
//! from a tag. So the failure lands after the tag exists, on an immutable
//! release, which is the most expensive moment to discover it. It is not
//! hypothetical: the same drift bit epistle, where `rcgen`, `time` and
//! `time-macros` each required 1.88 while `control` still said 1.85.
//!
//! This test is cheap and runs on every pull request, so the mismatch is
//! caught while it is still a one-line edit.

use std::fs;
use std::path::Path;

/// Parse `rust-version = "1.85"` out of the manifest.
fn manifest_rust_version(manifest: &str) -> String {
	manifest
		.lines()
		.find_map(|l| l.trim().strip_prefix("rust-version"))
		.and_then(|rest| rest.split('"').nth(1))
		.expect("Cargo.toml declares rust-version")
		.to_string()
}

/// Parse the version out of `rustc (>= 1.85)` in a `Build-Depends:` line.
///
/// The field can wrap across lines and list packages in any order, so this
/// looks for the `rustc (>= ...)` token wherever it appears rather than
/// assuming a position.
fn control_rustc_floor(control: &str) -> String {
	let at = control
		.find("rustc (>=")
		.expect("debian/control constrains rustc");
	let rest = &control[at..];
	let open = rest.find(">=").expect("the constraint has an operator") + 2;
	let close = rest.find(')').expect("the constraint is closed");
	rest[open..close].trim().to_string()
}

#[test]
fn the_debian_floor_matches_the_crate_rust_version() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml is readable");
	let control = fs::read_to_string(root.join("debian/control")).expect("control is readable");

	let crate_floor = manifest_rust_version(&manifest);
	let deb_floor = control_rustc_floor(&control);

	assert_eq!(
		deb_floor, crate_floor,
		"debian/control declares rustc (>= {deb_floor}) but Cargo.toml declares \
		 rust-version = \"{crate_floor}\". Update debian/control: apt checks \
		 that constraint before building, and the .deb is only built from a \
		 tag, so a mismatch surfaces on an immutable release."
	);
}

/// The parsers are what this test trusts, so pin them on inputs that differ
/// from today's files — otherwise a parser that always returned the same
/// string would pass the comparison above and prove nothing.
#[test]
fn the_parsers_read_the_values_rather_than_guessing_them() {
	assert_eq!(
		manifest_rust_version("[package]\nname = \"x\"\nrust-version = \"1.99\"\n"),
		"1.99"
	);
	assert_eq!(
		control_rustc_floor("Build-Depends: debhelper-compat (= 13), cargo, rustc (>= 1.42)\n"),
		"1.42"
	);
	// The field wraps in real control files, and rustc need not be last.
	assert_eq!(
		control_rustc_floor("Build-Depends: rustc (>= 2.0),\n cargo,\n debhelper\n"),
		"2.0"
	);
}
