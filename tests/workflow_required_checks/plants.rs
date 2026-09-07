//! Planted renames, and the negative control under them.
//!
//! `shape.rs` asserts that the real tree emits every required check. That
//! assertion is worth exactly what the walker under it is worth: one that
//! returned an empty list whatever it was given would pass against a tree
//! where nothing resolves at all, which is the failure mode the issue names.
//! So each case here plants ONE rename, in memory against a copy of the real
//! files, and requires the check that stopped being emitted to be reported by
//! name rather than by a count. The plants are the three shapes this
//! repository actually has: a caller job id, an inner job name in a reusable,
//! and a job defined directly in a workflow.

use std::collections::BTreeMap;

use crate::shape::{
	jobs_of, read_workflow, unresolved, workflows, workflows_dir, Job, REQUIRED_ON_MAIN,
};

/// The required checks that already do not resolve in the real tree.
///
/// Zero of them, unless something is broken, and `shape.rs` is the file that
/// says so. Every case here subtracts this, so a real rename fails the two
/// tests that are about the real tree and not the three that are about the
/// walker.
fn baseline() -> Vec<String> {
	unresolved(&workflows(&workflows_dir()), REQUIRED_ON_MAIN)
}

/// What the given plant, and nothing else, stops resolving.
///
/// The real tree is walked twice: once as it is, once with one file replaced
/// by the planted copy, and only the difference is returned. Asserting on the
/// whole list instead would make every plant here fail whenever a real rename
/// lands, which reports one defect four times and buries the one test that
/// was about it. `shape.rs` is where a real breakage belongs.
///
/// The caller passes the file it planted into and the planted text. Whether
/// the plant actually changed anything is asserted at the call site, since a
/// plant that silently matched nothing would leave the delta empty and the
/// case would pass for the wrong reason.
fn newly_unresolved(file: &str, planted: &str) -> Vec<String> {
	let baseline = baseline();
	let mut with_plant = workflows(&workflows_dir());
	with_plant.insert(file.to_string(), jobs_of(planted));
	unresolved(&with_plant, REQUIRED_ON_MAIN)
		.into_iter()
		.filter(|v| !baseline.contains(v))
		.collect()
}

/// A renamed caller job id is reported, by the name that stopped resolving.
///
/// The plant is in memory, against a copy of the real tree, so the case
/// exercises the same files CI reads without touching them. `rust` renamed to
/// `rustx` takes out all six `rust / *` checks at once and nothing else.
#[test]
fn a_renamed_caller_job_id_is_reported_by_name() {
	let dir = workflows_dir();
	let ci = read_workflow(&dir.join("ci.yml"));
	let needle = "  rust:\n    uses: ./.github/workflows/reusable-rust-ci.yml";
	assert!(
		ci.contains(needle),
		"ci.yml no longer carries the caller `rust:` this case plants against"
	);
	let planted = ci.replace(
		needle,
		"  rustx:\n    uses: ./.github/workflows/reusable-rust-ci.yml",
	);
	assert_ne!(planted, ci, "the plant must change the file it plants into");

	let gone = newly_unresolved("ci.yml", &planted);

	// Every `rust / *` check that resolves today, which is all of them unless
	// something is already broken, and only those: a plant is measured by
	// what it changed, not by the state of the tree it was planted into.
	let already = baseline();
	let expected: Vec<&str> = REQUIRED_ON_MAIN
		.iter()
		.copied()
		.filter(|c| c.starts_with("rust / "))
		.filter(|c| !already.iter().any(|v| v.starts_with(&format!("{c}:"))))
		.collect();
	assert_eq!(
		gone.len(),
		expected.len(),
		"every rust check that resolved before the plant must stop resolving \
		 after it, got:\n  {}",
		gone.join("\n  ")
	);
	for check in expected {
		assert!(
			gone.iter().any(|v| v.starts_with(&format!("{check}:"))),
			"{check} stopped being emitted and was not named. Got:\n  {}",
			gone.join("\n  ")
		);
	}
	assert!(
		gone.iter().all(|v| v.contains("job id 'rust'")),
		"the message must name the missing caller id"
	);
}

/// A renamed inner job name is reported, and only that one.
///
/// `Coverage` renamed to `Cov` in the reusable takes out `rust / Coverage`.
/// The other five `rust / *` checks share the caller id and must stay
/// resolved: a checker that reported all six here would be reacting to the
/// file changing rather than to the name.
#[test]
fn a_renamed_inner_job_name_is_reported_for_that_half_only() {
	let dir = workflows_dir();
	let reusable = read_workflow(&dir.join("reusable-rust-ci.yml"));
	let needle = "  coverage:\n    name: Coverage\n";
	assert!(
		reusable.contains(needle),
		"reusable-rust-ci.yml no longer carries `name: Coverage`"
	);
	let planted = reusable.replace(needle, "  coverage:\n    name: Cov\n");
	assert_ne!(planted, reusable, "the plant must change the file");

	let gone = newly_unresolved("reusable-rust-ci.yml", &planted);

	assert_eq!(
		gone.len(),
		1,
		"only rust / Coverage loses its job, got:\n  {}",
		gone.join("\n  ")
	);
	assert!(
		gone[0].starts_with("rust / Coverage:") && gone[0].contains("named 'Coverage'"),
		"the message must name the check and the missing inner name, got: {}",
		gone[0]
	);
}

/// A removed direct job is reported. This is the shape with no slash, and it
/// resolves through a different branch of the walker than the two above.
#[test]
fn a_renamed_direct_job_name_is_reported_by_name() {
	let dir = workflows_dir();
	let lane = read_workflow(&dir.join("podman-lane.yml"));
	let needle = "    name: Supported Podman majors\n";
	assert!(
		lane.contains(needle),
		"podman-lane.yml no longer carries `name: Supported Podman majors`"
	);
	let planted = lane.replace(needle, "    name: Supported Podman versions\n");
	assert_ne!(planted, lane, "the plant must change the file");

	let gone = newly_unresolved("podman-lane.yml", &planted);

	assert_eq!(
		gone,
		vec!["Supported Podman majors: no workflow has a job named \
			 'Supported Podman majors'"
			.to_string()],
		"the one-part shape must be reported too"
	);
}

/// The negative control for the whole file: a tree where nothing resolves has
/// to report EVERY required check, and a tree that resolves has to report
/// none. A checker that returned an empty list whatever it was given would
/// satisfy the two real-tree tests above and prove nothing, which is the
/// failure mode the issue names.
#[test]
fn the_walker_reports_everything_against_a_tree_where_nothing_resolves() {
	let empty: BTreeMap<String, BTreeMap<String, Job>> = BTreeMap::new();
	assert_eq!(
		unresolved(&empty, REQUIRED_ON_MAIN).len(),
		REQUIRED_ON_MAIN.len(),
		"an empty tree emits none of the required checks"
	);

	// A tree whose only workflow is a reusable carrying every inner name.
	// Nothing calls it, so nothing is emitted: a reusable is not a caller,
	// and a walker that accepted one would pass a tree GitHub reports
	// nothing for.
	let mut orphan = BTreeMap::new();
	orphan.insert(
		"reusable-everything.yml".to_string(),
		jobs_of(
			"\
jobs:
  a:
    name: Coverage
  b:
    name: line limit
",
		),
	);
	let gone = unresolved(&orphan, &["rust / Coverage", "line-limit / line limit"]);
	assert_eq!(
		gone.len(),
		2,
		"a reusable nobody calls emits nothing, got:\n  {}",
		gone.join("\n  ")
	);

	// And the positive half, so the two above cannot both be satisfied by a
	// walker that reports every check unconditionally.
	let mut resolving = BTreeMap::new();
	resolving.insert(
		"caller.yml".to_string(),
		jobs_of(
			"\
jobs:
  line-limit:
    uses: ./.github/workflows/reusable-line-limit.yml
  standalone:
    name: Supported Podman majors
",
		),
	);
	resolving.insert("reusable-line-limit.yml".to_string(), {
		jobs_of(
			"\
jobs:
  line-limit:
    name: line limit
",
		)
	});
	assert!(
		unresolved(
			&resolving,
			&["line-limit / line limit", "Supported Podman majors"]
		)
		.is_empty(),
		"both shapes resolve in a tree built to resolve them"
	);
}

/// The parser under all of it, pinned on the three shapes it has to reject.
/// Each one is present in this repository's workflows.
#[test]
fn the_parser_reads_job_names_and_not_the_prose_around_them() {
	let yml = "\
name: Coverage
on:
  pull_request:
jobs:
  # coverage:
  #   name: Coverage
  rust:
    uses: ./.github/workflows/reusable-rust-ci.yml
    with:
      coverage-threshold: 76
  direct:
    name: Supported Podman majors
    steps:
      - name: Coverage
        run: cargo llvm-cov
";
	let jobs = jobs_of(yml);
	assert_eq!(
		jobs.keys().collect::<Vec<_>>(),
		vec!["direct", "rust"],
		"only the two real job ids, not a commented one and not the workflow's \
		 own top-level name"
	);
	assert_eq!(
		jobs["rust"].name, None,
		"the caller carries no name of its own"
	);
	assert_eq!(
		jobs["rust"].uses.as_deref(),
		Some("./.github/workflows/reusable-rust-ci.yml")
	);
	assert_eq!(
		jobs["direct"].name.as_deref(),
		Some("Supported Podman majors"),
		"a step's `- name:` must not overwrite the job's"
	);
}
