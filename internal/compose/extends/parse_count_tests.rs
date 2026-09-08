// Behaviour test for #1746 entry 3: `extends: {file:}` is re-read and
// re-parsed once per referencing service.
//
// Without the cache, twenty services each saying
// `extends: { service: base, file: common.yml }` read and parse
// `common.yml` twenty times. Each parse holds the YAML value tree
// (the bytes are capped at 16 MiB but the parsed tree is much larger)
// while the merge runs, so the concurrent twenty peak at 5.8 GB and
// the process aborts. The cross-review corrected the brief: the
// fix is cache + bound, not a parse-count limit, so a test that
// counts the parses is the right shape.

use std::fs;

use tempfile::tempdir;

use super::super::parse_file;
use super::{parse_file_inner_call_count, reset_parse_file_inner_counter};

/// Twenty services in the parent, every one extending the same base
/// service in the same external file. Twenty was the number the issue
/// quotes; using the literal figure means a fix that breaks at
/// nineteen still gets caught.
const REFERRERS: usize = 20;

#[test]
fn extends_file_is_parsed_at_most_once_per_shared_external_file() {
	let dir = tempdir().expect("tempdir");

	// `common.yml` declares the base service. Its size is small on
	// disk (well under the 16 MiB per-file cap) but the parsed YAML
	// value tree is what the issue is about: twenty concurrent trees
	// peak at 5.8 GB. The test does not need a 16 MiB file to count
	// the parses.
	let common = dir.path().join("common.yml");
	fs::write(
		&common,
		"services:\n  base:\n    image: postgres:16\n    environment:\n      SHARED: common\n",
	)
	.expect("write common.yml");

	// The parent file declares REFERRERS services, every one
	// extending the same `base` in `common.yml`. Without the cache,
	// each iteration of `resolve_one_extends` for an `extends.file`
	// re-reads and re-parses the external file.
	let mut parent = String::from("services:\n");
	for i in 0..REFERRERS {
		use std::fmt::Write as _;
		writeln!(
			&mut parent,
			"  referrer_{i}:\n    extends:\n      service: base\n      file: common.yml\n    environment:\n      LOCAL: {i}\n",
		)
		.expect("write parent yaml");
	}
	let parent_path = dir.path().join("parent.yml");
	fs::write(&parent_path, &parent).expect("write parent.yml");

	// Reset the counter so it measures only this top-level parse, not
	// the parse of `common.yml` that happened when the test resolved
	// its own absolute path during canonicalize.
	reset_parse_file_inner_counter();
	let parsed = parse_file(&parent_path).expect("parse parent");
	let count = parse_file_inner_call_count();

	// One parse of the parent plus one parse of `common.yml` (cached
	// for every referrer after the first). Before the fix the count
	// was 1 + REFERRERS.
	assert_eq!(
		count, 1,
		"`extends.file` was parsed {count} times for {REFERRERS} referencing services; \
		 expected exactly 1 (#1746). The cache collapsed re-reads across all \
		 referrers.",
	);

	// Sanity: the merge actually happened, the parent services carry
	// the inherited image AND their own local value.
	let svc = &parsed.services[&format!("referrer_{}", REFERRERS - 1)];
	assert_eq!(svc.image.as_deref(), Some("postgres:16"));
}

/// Two distinct external files are each parsed exactly once. Caching
/// by canonical path means distinct paths do not collide; a fix that
/// keyed by file name (without canonicalize) would over-merge and
/// reject this shape.
#[test]
fn extends_file_caches_per_path() {
	let dir = tempdir().expect("tempdir");

	fs::write(
		dir.path().join("common_a.yml"),
		"services:\n  base:\n    image: postgres:16\n",
	)
	.expect("write common_a");
	fs::write(
		dir.path().join("common_b.yml"),
		"services:\n  base:\n    image: redis:7\n",
	)
	.expect("write common_b");
	let parent = "services:\n  a:\n    extends:\n      service: base\n      file: common_a.yml\n  b:\n    extends:\n      service: base\n      file: common_b.yml\n";
	let parent_path = dir.path().join("parent.yml");
	fs::write(&parent_path, parent).expect("write parent");

	reset_parse_file_inner_counter();
	let parsed = parse_file(&parent_path).expect("parse parent");
	let count = parse_file_inner_call_count();

	// One parse per distinct external file, regardless of how many
	// services reference each. The parent itself is the one parse
	// that does not count here (it was parsed before
	// `resolve_all_extends` runs).
	assert_eq!(
		count, 2,
		"two distinct external files must produce two parses, got {count}"
	);
	assert_eq!(parsed.services["a"].image.as_deref(), Some("postgres:16"));
	assert_eq!(parsed.services["b"].image.as_deref(), Some("redis:7"));
}

#[test]
fn an_unrelated_broken_service_does_not_fail_the_reference() {
	// The cache used to resolve every service in a referenced file before
	// caching it, so a project referencing one valid service failed when an
	// unrelated service in that file extended something missing. A file you
	// do not control could break your build over a service you never asked
	// for.
	let dir = tempdir().expect("tempdir");
	let base = dir.path().join("base.yml");
	fs::write(
		&base,
		"services:\n  base:\n    image: alpine:3.20\n  broken:\n    extends:\n      service: does-not-exist\n",
	)
	.expect("write base.yml");

	let good = dir.path().join("good.yml");
	fs::write(
		&good,
		"services:\n  app:\n    extends:\n      service: base\n      file: base.yml\n",
	)
	.expect("write good.yml");
	parse_file(&good).expect("referencing a valid service must not fail over an unrelated one");

	// And the broken service is still an error when it is the one asked for,
	// so this does not simply stop reporting the failure.
	let bad = dir.path().join("bad.yml");
	fs::write(
		&bad,
		"services:\n  app:\n    extends:\n      service: broken\n      file: base.yml\n",
	)
	.expect("write bad.yml");
	assert!(
		parse_file(&bad).is_err(),
		"asking for the broken service must still fail"
	);
}
