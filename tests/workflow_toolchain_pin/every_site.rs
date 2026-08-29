//! Every site that writes the toolchain, not only the two that build the
//! `.deb`.
//!
//! The pair in `deb_pair.rs` was written when `Glyndor/.github` held the
//! toolchain default and this repository named it twice. #1568 copied the
//! reusables in, so the default is here now, three times, and the callers
//! that pass it add six more. Nine sites write the stable pin and nothing
//! derives one from another, which is one datum in nine places updated one
//! at a time.
//!
//! `pin-watch.yml` reads the same set every Monday and compares the agreed
//! value against upstream stable. That is the half this file cannot do,
//! since it needs the network. This is the half the watcher should not be
//! relied on for: a weekly cron reports a drift up to seven days after the
//! pull request that introduced it, and by then it is no longer a one-line
//! edit in a branch somebody still has open. The overlap between the two is
//! deliberate and each says so, because a check that leans on another check
//! is one deletion away from silence.

use std::fs;
use std::path::Path;

#[derive(Debug, PartialEq)]
struct PinSite {
	file: String,
	key: String,
	value: String,
}

/// Sites that pin a toolchain deliberately different from the stable one.
///
/// Each names a KIND rather than only a reason, and the kind is checked
/// against the value. An exemption keyed on where it sits admits whatever
/// lands there, which is how a rewritten pin walks past a rule written to let
/// one specific pin through.
const NOT_STABLE: &[(&str, &str, &str)] = &[
	(
		"reusable-rust-fuzz.yml",
		"toolchain.default",
		"nightly", // cargo-fuzz requires a nightly toolchain
	),
	(
		"asset-contract.yml",
		"RUST_TOOLCHAIN",
		"msrv", // mirrors the self_test in internal/update/install.rs
	),
];

/// Collect every toolchain pin written in a workflow file.
///
/// Three spellings: the `RUST_TOOLCHAIN` env a job hands to rustup, the
/// `rust-toolchain` input a caller passes, and the `default:` on a reusable's
/// own `toolchain:` input. A value containing `${{` is a pass-through, not a
/// pin - it resolves at whichever caller supplied it, and that caller is
/// already one of these sites.
fn toolchain_pin_sites(file: &str, workflow: &str) -> Vec<PinSite> {
	let mut sites = Vec::new();
	let mut in_toolchain_input: Option<usize> = None;

	for line in workflow.lines() {
		let trimmed = line.trim_start();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		let indent = line.len() - trimmed.len();

		// Leave the `toolchain:` input block as soon as a key at the same or
		// lower indent appears, so a `default:` belonging to the next input
		// is not attributed to this one.
		if let Some(open_indent) = in_toolchain_input {
			if indent <= open_indent {
				in_toolchain_input = None;
			} else if let Some(rest) = trimmed.strip_prefix("default:") {
				let value = rest.trim().trim_matches('"');
				if !value.is_empty() && !value.contains("${{") {
					sites.push(PinSite {
						file: file.to_string(),
						key: "toolchain.default".to_string(),
						value: value.to_string(),
					});
				}
				in_toolchain_input = None;
				continue;
			}
		}

		if trimmed == "toolchain:" {
			in_toolchain_input = Some(indent);
			continue;
		}

		for key in ["RUST_TOOLCHAIN", "rust-toolchain"] {
			if let Some(rest) = trimmed.strip_prefix(key).and_then(|r| r.strip_prefix(':')) {
				let value = rest.trim().trim_matches('"');
				if !value.is_empty() && !value.contains("${{") {
					sites.push(PinSite {
						file: file.to_string(),
						key: key.to_string(),
						value: value.to_string(),
					});
				}
				break;
			}
		}
	}
	sites
}

/// Read every workflow and collect its pin sites, sorted for a stable message.
fn all_pin_sites() -> Vec<PinSite> {
	let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
	let mut files: Vec<_> = fs::read_dir(&dir)
		.expect(".github/workflows is readable")
		.map(|e| e.expect("directory entry").path())
		.filter(|p| p.extension().is_some_and(|e| e == "yml" || e == "yaml"))
		.collect();
	files.sort();

	let mut sites = Vec::new();
	for path in files {
		let name = path
			.file_name()
			.expect("workflow has a file name")
			.to_string_lossy()
			.into_owned();
		let body = fs::read_to_string(&path).expect("workflow is readable");
		sites.extend(toolchain_pin_sites(&name, &body));
	}
	sites
}

/// A stable pin is `MAJOR.MINOR`, all digits either side of one dot.
fn looks_stable(value: &str) -> bool {
	let mut parts = value.split('.');
	match (parts.next(), parts.next(), parts.next()) {
		(Some(a), Some(b), None) => {
			!a.is_empty()
				&& !b.is_empty()
				&& a.bytes().all(|c| c.is_ascii_digit())
				&& b.bytes().all(|c| c.is_ascii_digit())
		}
		_ => false,
	}
}

fn declared_kind(site: &PinSite) -> Option<&'static str> {
	NOT_STABLE
		.iter()
		.find(|(file, key, _)| *file == site.file && *key == site.key)
		.map(|(_, _, kind)| *kind)
}

#[test]
fn every_toolchain_pin_site_is_classified() {
	let sites = all_pin_sites();
	let mut unclassified = Vec::new();
	for site in &sites {
		match declared_kind(site) {
			None if looks_stable(&site.value) => {}
			None => unclassified.push(format!(
				"{} {} pins {:?}, which is neither MAJOR.MINOR nor a listed exception",
				site.file, site.key, site.value
			)),
			Some("nightly") if site.value.starts_with("nightly-") => {}
			Some("msrv") if looks_stable(&site.value) => {}
			Some(kind) => unclassified.push(format!(
				"{} {} is listed as the {kind} pin and now reads {:?}, which no \
				 longer looks like one",
				site.file, site.key, site.value
			)),
		}
	}
	assert!(
		unclassified.is_empty(),
		"a toolchain pin has to be classified deliberately, because the \
		 agreement test treats anything unlisted as a stable pin:\n  {}",
		unclassified.join("\n  ")
	);
}

#[test]
fn every_stable_toolchain_pin_declares_the_same_version() {
	let sites = all_pin_sites();
	let stable: Vec<&PinSite> = sites
		.iter()
		.filter(|s| declared_kind(s).is_none())
		.collect();
	assert!(
		!stable.is_empty(),
		"no stable toolchain pin found; see every_toolchain_pin_site_is_classified"
	);

	let mut distinct: Vec<&str> = stable.iter().map(|s| s.value.as_str()).collect();
	distinct.sort_unstable();
	distinct.dedup();

	assert_eq!(
		distinct.len(),
		1,
		"the {} stable toolchain pins do not agree ({}). Nothing derives one \
		 from another, so a release can be built by a compiler CI never ran. \
		 Sites: {stable:#?}",
		stable.len(),
		distinct.join(", ")
	);
}

#[test]
fn every_msrv_copy_agrees_with_the_declared_msrv() {
	let sites = all_pin_sites();
	let ci_yml =
		fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"))
			.expect("ci.yml is readable");
	let declared = ci_yml
		.lines()
		.map(str::trim_start)
		.filter(|l| !l.starts_with('#'))
		.find_map(|l| l.strip_prefix("msrv:"))
		.map(|rest| rest.trim().trim_matches('"').to_string())
		.expect("ci.yml passes an msrv input");

	for site in sites.iter().filter(|s| declared_kind(s) == Some("msrv")) {
		assert_eq!(
			site.value, declared,
			"{} {} carries {:?} as a second copy of the MSRV while ci.yml \
			 declares {declared:?}. The copy exists to mirror the self_test in \
			 internal/update/install.rs, and nothing moves it when the floor \
			 moves.",
			site.file, site.key, site.value
		);
	}
}

/// Same reason as the parser test above: a scanner that returned a fixed
/// answer would satisfy the assertions and prove nothing. Pin it on input
/// that differs from today's files, including the shapes it must NOT count.
#[test]
fn the_pin_scanner_reads_the_values_rather_than_guessing_them() {
	let yml = "\
on:
  workflow_call:
    inputs:
      working-directory:
        default: \".\"
      toolchain:
        description: pin
        type: string
        default: \"3.14\"
      coverage-threshold:
        default: \"45\"
jobs:
  a:
    steps:
      # RUST_TOOLCHAIN: \"9.9\" in a comment is not a pin
      - env:
          RUST_TOOLCHAIN: \"2.72\"
  b:
    with:
      rust-toolchain: \"1.11\"
  c:
    steps:
      - env:
          RUST_TOOLCHAIN: ${{ inputs.rust-toolchain }}
";
	let sites = toolchain_pin_sites("sample.yml", yml);
	let seen: Vec<(&str, &str)> = sites
		.iter()
		.map(|s| (s.key.as_str(), s.value.as_str()))
		.collect();
	assert_eq!(
		seen,
		vec![
			("toolchain.default", "3.14"),
			("RUST_TOOLCHAIN", "2.72"),
			("rust-toolchain", "1.11"),
		],
		"the scanner must take the toolchain input's own default and skip the \
		 neighbouring inputs' defaults, skip a value named only in a comment, \
		 and skip a pass-through expression"
	);

	assert!(looks_stable("1.98"));
	assert!(looks_stable("1.85"));
	assert!(!looks_stable("nightly-2026-07-16"));
	assert!(!looks_stable("stable"));
	assert!(!looks_stable("1.98.0"));
	assert!(!looks_stable("1."));
}

/// A count would be a copy of the tree, stale the day a site is added. The
/// property is that the scanner does not go blind on a FILE: if a workflow
/// writes a toolchain on a line that is not a comment and not a
/// pass-through, at least one site must come back for that file. A scanner
/// that quietly stopped parsing one shape would leave the agreement test
/// passing over a smaller set than it claims, which is the failure this
/// catches and a total cannot.
#[test]
fn the_scanner_sees_every_file_that_writes_a_toolchain() {
	let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
	let mut blind = Vec::new();

	for entry in fs::read_dir(&dir).expect(".github/workflows is readable") {
		let path = entry.expect("directory entry").path();
		if !path.extension().is_some_and(|e| e == "yml" || e == "yaml") {
			continue;
		}
		let name = path
			.file_name()
			.expect("workflow has a file name")
			.to_string_lossy()
			.into_owned();
		let body = fs::read_to_string(&path).expect("workflow is readable");

		// Deliberately cruder than the scanner: any non-comment line naming
		// one of the keys and carrying a value. It over-matches on purpose,
		// so it can disagree with the scanner rather than echo it. A bare
		// `rust-toolchain:` with nothing after it is the input DECLARATION in
		// reusable-rust-debian.yml, which pins nothing; the value it takes is
		// pinned at the caller, and that caller is its own site.
		let mentions = body.lines().map(str::trim_start).any(|l| {
			if l.starts_with('#') || l.contains("${{") {
				return false;
			}
			["RUST_TOOLCHAIN:", "rust-toolchain:"]
				.iter()
				.filter_map(|k| l.strip_prefix(k))
				.any(|rest| !rest.trim().is_empty())
		});
		if mentions && toolchain_pin_sites(&name, &body).is_empty() {
			blind.push(name);
		}
	}
	assert!(
		blind.is_empty(),
		"the scanner returned nothing for {blind:?}, which write a toolchain \
		 on a line that is neither a comment nor a pass-through"
	);
}

/// Not an assertion about a value: it prints the census the two tests above
/// depend on, so a run that silently stopped seeing sites is visible in the
/// log rather than only in a count.
#[test]
fn the_pin_census_is_visible_in_the_run() {
	let sites = all_pin_sites();
	for site in &sites {
		let kind = declared_kind(site).unwrap_or("stable");
		println!(
			"{:<32} {:<18} {:<20} {kind}",
			site.file, site.key, site.value
		);
	}
	println!("{} toolchain pin site(s)", sites.len());
}
