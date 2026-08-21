//! The two workflow pins that build the package must agree, and that agreed
//! toolchain must satisfy `Cargo.toml`'s `rust-version`.
//!
//! Before this branch, the constraint lived in `debian/control` as
//! `Build-Depends: rustc (>= X)`, hand-written against `rust-version` rather
//! than derived from it. Nothing kept the two in step, and the mismatch
//! only surfaced on an immutable release - the most expensive moment to find
//! it. That is what bit epistle: `rcgen`, `time` and `time-macros` each
//! required 1.88 against `control` still saying 1.85, and the build died on
//! a resolution the manifest never mentioned.
//!
//! That constraint was removed in #1525 because the package now builds for a
//! musl target, and Debian ships no musl standard library. The build is
//! instead driven by the toolchain the workflows install and pin. Two new
//! relationships are the ones that must hold:
//!
//! 1. The two pins that govern the package build must declare the **same**
//!    toolchain. A drift between them reintroduces `Glyndor/podup#1487` -
//!    the package compiled by one toolchain, validated by the other.
//! 2. That agreed toolchain must **satisfy** `Cargo.toml`'s `rust-version`.
//!    A dependency can raise `rust-version` on an ordinary `cargo update`,
//!    and the `.deb` is only built from a tag, so the failure lands on an
//!    immutable release.
//!
//! The pins are:
//!
//! - `.github/workflows/release.yml`, in the `build-deb` job's
//!   `RUST_TOOLCHAIN` env.
//! - `.github/workflows/debian-build.yml`, in the `rust-toolchain:` input
//!   to the reusable workflow.
//!
//! This test is cheap and runs on every pull request, so the mismatch is
//! caught while it is still a one-line edit.

use std::cmp::Ordering;
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

/// Parse the `RUST_TOOLCHAIN: "X"` env value from the `build-deb:` job of
/// `release.yml`.
///
/// The `build:` and `release:` jobs each pin their own toolchain too, and the
/// parser must scope to `build-deb:` so a future drift between siblings is
/// not papered over. The scan stops at the next sibling key at the same
/// indent as `build-deb:`, which is how a new job in the same `jobs:` block
/// announces itself.
fn release_yml_build_deb_toolchain(workflow: &str) -> String {
	let lines: Vec<&str> = workflow.lines().collect();
	let deb_idx = lines
		.iter()
		.position(|l| l.trim() == "build-deb:")
		.expect("release.yml has a build-deb job");
	let deb_indent = lines[deb_idx].len() - lines[deb_idx].trim_start().len();

	for line in &lines[deb_idx + 1..] {
		let trimmed = line.trim_start();
		if trimmed.is_empty() {
			continue;
		}
		let indent = line.len() - trimmed.len();
		if indent <= deb_indent {
			break;
		}
		if let Some(rest) = trimmed.strip_prefix("RUST_TOOLCHAIN:") {
			return rest.trim().trim_matches('"').to_string();
		}
	}
	panic!("RUST_TOOLCHAIN not found under the build-deb job of release.yml");
}

/// Parse the `rust-toolchain:` input value from `debian-build.yml`.
fn debian_build_yml_toolchain(workflow: &str) -> String {
	for line in workflow.lines() {
		let trimmed = line.trim_start();
		if let Some(rest) = trimmed.strip_prefix("rust-toolchain:") {
			return rest.trim().trim_matches('"').to_string();
		}
	}
	panic!("debian-build.yml has a rust-toolchain input");
}

/// Compare two Rust version strings (major[.minor[.patch]]) numerically.
///
/// Lexicographic order is wrong here: `"1.9"` sorts above `"1.85"`, which is
/// the failure a string compare introduces. Split on `.` and parse each
/// component as a `u32`, then pad the shorter side with zeros so a trailing
/// `.0` is equal to the bare form (`"1.85.0"` == `"1.85"`).
fn cmp_version(a: &str, b: &str) -> Ordering {
	let parse = |v: &str| -> Vec<u32> { v.split('.').map(|s| s.parse().unwrap_or(0)).collect() };
	let mut av = parse(a);
	let mut bv = parse(b);
	let max_len = av.len().max(bv.len());
	av.resize(max_len, 0);
	bv.resize(max_len, 0);
	av.cmp(&bv)
}

#[test]
fn the_two_workflow_pins_declare_the_same_toolchain() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let release_yml = fs::read_to_string(root.join(".github/workflows/release.yml"))
		.expect("release.yml is readable");
	let debian_build_yml = fs::read_to_string(root.join(".github/workflows/debian-build.yml"))
		.expect("debian-build.yml is readable");

	let deb = release_yml_build_deb_toolchain(&release_yml);
	let reusable = debian_build_yml_toolchain(&debian_build_yml);

	assert_eq!(
		deb, reusable,
		"release.yml's build-deb job pins {deb:?} but debian-build.yml pins \
		 {reusable:?}. A drift between the two means the package is built by \
		 a compiler CI does not validate with - that is Glyndor/podup#1487 \
		 stated as a YAML mismatch."
	);
}

#[test]
fn the_pinned_toolchain_satisfies_the_crate_floor() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let release_yml = fs::read_to_string(root.join(".github/workflows/release.yml"))
		.expect("release.yml is readable");
	let debian_build_yml = fs::read_to_string(root.join(".github/workflows/debian-build.yml"))
		.expect("debian-build.yml is readable");
	let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("Cargo.toml is readable");

	let deb = release_yml_build_deb_toolchain(&release_yml);
	let reusable = debian_build_yml_toolchain(&debian_build_yml);
	let crate_floor = manifest_rust_version(&manifest);

	// The two pins have to agree before this check is meaningful: an answer
	// about "is the agreed toolchain >= rust-version" when there is no agreed
	// toolchain would just point at the wrong failure.
	assert_eq!(
		deb, reusable,
		"the two workflow pins disagree before the rust-version check can run: \
		 build-deb pins {deb:?}, reusable pins {reusable:?}. Fix the pin drift \
		 first; that is the relationship Glyndor/podup#1487 is about."
	);

	assert_ne!(
		cmp_version(&deb, &crate_floor),
		Ordering::Less,
		"the agreed workflow toolchain is {deb}, which is below Cargo.toml's \
		 rust-version = \"{crate_floor}\". The workflows build with a compiler \
		 that does not satisfy the crate, and the .deb is only built from a \
		 tag, so the failure lands on an immutable release."
	);
}

/// The parsers are what these tests trust, so pin them on inputs that differ
/// from today's files - otherwise a parser that always returned the same
/// string would pass the assertions above and prove nothing.
#[test]
fn the_parsers_read_the_values_rather_than_guessing_them() {
	// manifest_rust_version: pin on an input that differs from the real Cargo.toml.
	assert_eq!(
		manifest_rust_version("[package]\nname = \"x\"\nrust-version = \"1.99\"\n"),
		"1.99"
	);

	// release_yml_build_deb_toolchain: the build and release jobs also pin a
	// toolchain; the parser must scope to build-deb and ignore them.
	let yml_three_jobs = "\
jobs:
  build:
    steps:
      - env:
          RUST_TOOLCHAIN: \"1.50\"
  build-deb:
    steps:
      - env:
          RUST_TOOLCHAIN: \"2.7\"
  release:
    steps:
      - env:
          RUST_TOOLCHAIN: \"1.51\"
";
	assert_eq!(release_yml_build_deb_toolchain(yml_three_jobs), "2.7");

	// release_yml_build_deb_toolchain: a comment that names the same token
	// with a fake value must not be returned.
	let yml_with_comment = "\
jobs:
  build-deb:
    # comment with RUST_TOOLCHAIN: \"1.66\" in it
    steps:
      - env:
          RUST_TOOLCHAIN: \"1.42\"
  release:
    steps:
      - env:
          RUST_TOOLCHAIN: \"1.43\"
";
	assert_eq!(release_yml_build_deb_toolchain(yml_with_comment), "1.42");

	// debian_build_yml_toolchain: extracts the rust-toolchain input value,
	// including with a different surrounding structure than today's file.
	let deb_yml = "\
jobs:
  debian:
    uses: Glyndor/.github/.github/workflows/rust-debian.yml@e70ff47fd8aefa0f054846815f19b98768d61122
    with:
      package-name: podup
      check-vendored: true
      offline-cargo-args: \"--no-default-features --features watch,completions\"
      rust-toolchain: \"1.85\"
      rust-target: x86_64-unknown-linux-musl
";
	assert_eq!(debian_build_yml_toolchain(deb_yml), "1.85");

	// cmp_version: the case a string compare gets wrong. \"1.9\" sorts above
	// \"1.85\" lexicographically, which is exactly the false-positive this
	// comparison exists to prevent.
	assert_eq!(cmp_version("1.9", "1.85"), Ordering::Less);
	assert_eq!(cmp_version("1.85", "1.9"), Ordering::Greater);
	assert_eq!(cmp_version("1.85", "1.85"), Ordering::Equal);
	// major.minor.patch forms: trailing .0 equals bare major.minor, and any
	// higher patch is greater.
	assert_eq!(cmp_version("1.85.0", "1.85"), Ordering::Equal);
	assert_eq!(cmp_version("1.85.1", "1.85"), Ordering::Greater);
	// Cross-major: a higher major always wins, regardless of the minor.
	assert_eq!(cmp_version("2.0", "1.99"), Ordering::Greater);
}
