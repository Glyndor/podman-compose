//! The two workflows that build the `.deb` must build it in the same image.
//!
//! `release.yml`'s `build-deb` job names a `container:`, and
//! `reusable-rust-debian.yml` carries a `debian-image` default that the
//! `debian-build` lane takes on every pull request. A `container:` needs a
//! literal and cannot read the reusable's default, so the value exists twice
//! and nothing derives one from the other.
//!
//! They were different from before #1568 until #1583, by an image and six
//! weeks. The consequence is not cosmetic: the `.deb` that ships was built in
//! an environment no pull request had exercised, and the release is immutable,
//! so the first place a difference could surface was the one place it cannot
//! be corrected.
//!
//! `pin-watch.yml` reads both and reports them disagreeing, and that is a
//! weekly cron. This is the same assertion at pull-request time, which is
//! while the fix is still a one-line edit in an open branch. The overlap is
//! deliberate; both files say so.

use std::fs;
use std::path::Path;

/// Pull the pinned image out of a workflow.
///
/// `scope` is the key whose block the value lives in, or `None` when the key
/// is at the top level of a job (`container:`). Scoping matters: the first
/// draft took the first `default:` in the file, which in
/// `reusable-rust-debian.yml` belongs to a different input and reads
/// `ubuntu-latest`. Both assertions below failed on the first run and said so
/// by name, which is the argument for writing the test before trusting the
/// parser under it.
///
/// Comment lines are dropped first. A comment quoting a digest would satisfy
/// a bare search for the shape, and this file exists because a comment about
/// these two values was wrong for six weeks.
fn pinned_image(workflow: &str, scope: Option<&str>, key: &str) -> String {
	let mut inside = scope.is_none();
	let mut scope_indent = 0usize;

	for line in workflow.lines() {
		let trimmed = line.trim_start();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		let indent = line.len() - trimmed.len();

		if let Some(name) = scope {
			if !inside {
				if trimmed == name {
					inside = true;
					scope_indent = indent;
				}
				continue;
			}
			// A key at the same or lower indent closes the block, so a
			// `default:` belonging to the next input is not attributed here.
			if indent <= scope_indent {
				break;
			}
		}

		if let Some(rest) = trimmed.strip_prefix(key) {
			return rest.trim().trim_matches('"').to_string();
		}
	}
	panic!(
		"no `{key}` line pinning an image{}",
		match scope {
			Some(name) => format!(" under `{name}`"),
			None => String::new(),
		}
	)
}

fn read(name: &str) -> String {
	let path = Path::new(env!("CARGO_MANIFEST_DIR"))
		.join(".github/workflows")
		.join(name);
	fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name} is readable: {e}"))
}

#[test]
fn both_deb_builds_pin_the_same_debian_image() {
	let release = pinned_image(&read("release.yml"), None, "container:");
	let lane = pinned_image(
		&read("reusable-rust-debian.yml"),
		Some("debian-image:"),
		"default:",
	);

	assert_eq!(
		release, lane,
		"release.yml builds the published .deb in {release:?} while the \
		 debian-build lane builds it in {lane:?}. A difference means the \
		 package that ships was never built in an environment any pull \
		 request exercised, and a published release cannot be corrected."
	);
}

#[test]
fn the_pinned_image_is_a_digest_and_not_a_moving_tag() {
	for (name, scope, key) in [
		("release.yml", None, "container:"),
		(
			"reusable-rust-debian.yml",
			Some("debian-image:"),
			"default:",
		),
	] {
		let pinned = pinned_image(&read(name), scope, key);
		let (image, digest) = pinned
			.split_once("@sha256:")
			.unwrap_or_else(|| panic!("{name} pins {pinned:?}, which carries no sha256 digest"));
		assert_eq!(
			image, "debian:trixie",
			"{name} pins {image:?}. Sid was a moving tag: two releases built \
			 from identical source could land in different environments with \
			 nothing in history recording which."
		);
		assert!(
			digest.len() == 64 && digest.bytes().all(|c| c.is_ascii_hexdigit()),
			"{name} pins {digest:?}, which is not a 64-character hex digest"
		);
	}
}

/// The parser is what both tests trust, so pin it on input that differs from
/// today's files. Otherwise a parser that always returned the same string
/// would satisfy the equality above and prove nothing.
#[test]
fn the_parser_reads_the_value_rather_than_guessing_it() {
	let yml = "\
jobs:
  build-deb:
    # container: debian:trixie@sha256:0000000000000000000000000000000000000000000000000000000000000000
    container: debian:trixie@sha256:1111111111111111111111111111111111111111111111111111111111111111
";
	assert_eq!(
		pinned_image(yml, None, "container:"),
		"debian:trixie@sha256:1111111111111111111111111111111111111111111111111111111111111111",
		"a digest named only in a comment must not be returned"
	);

	// The case the first draft got wrong: another input defaults ahead of
	// debian-image, and an unscoped search returns that one.
	let reusable = "\
      runner:
        default: \"ubuntu-latest\"
      debian-image:
        description: Container image to build in
        default: \"debian:trixie@sha256:2222222222222222222222222222222222222222222222222222222222222222\"
      arch:
        default: \"amd64\"
";
	assert_eq!(
		pinned_image(reusable, Some("debian-image:"), "default:"),
		"debian:trixie@sha256:2222222222222222222222222222222222222222222222222222222222222222",
		"the value must come from debian-image's own block, not from whichever \
		 input defaults first"
	);
}
