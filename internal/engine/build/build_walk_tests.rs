// Test for #1746 entry 6: the build-context walk used to enumerate
// every entry under an ignored directory and rely on the per-file
// filter to drop them. The walk cost (the number of `read_dir` calls
// the OS sees) was paid before the filter had a chance to skip. The
// new walker prunes a directory whose ignore pattern covers it, so
// the same input is cheaper to build. The test counts `read_dir`
// calls inside the walk (a thread-local `#[cfg(test)]` counter,
// compiled out of release builds) and asserts the walk now does the
// minimum amount of work for an ignored subtree.

use super::context::build_context_tar;
use crate::engine::walk::{reset_walk_counter, walk_count};

#[test]
fn build_context_walk_does_not_enumerate_ignored_subtrees() {
	let dir = tempfile::tempdir().unwrap();
	// A 200-file `target/` the ignore pattern drops wholesale.
	let target = dir.path().join("target");
	std::fs::create_dir(&target).unwrap();
	for i in 0..200 {
		std::fs::write(target.join(format!("obj{i}")), b"x").unwrap();
	}
	// A 5-file `src/` the ignore pattern leaves intact, so the walk
	// still has to descend.
	let src = dir.path().join("src");
	std::fs::create_dir(&src).unwrap();
	for i in 0..5 {
		std::fs::write(src.join(format!("file{i}")), b"y").unwrap();
	}
	// `.dockerignore` drops the whole target subtree.
	std::fs::write(dir.path().join(".dockerignore"), "target\n").unwrap();
	// The Dockerfile has to exist; the walker forces it in.
	std::fs::write(dir.path().join("Dockerfile"), "FROM scratch\nCOPY . /\n").unwrap();

	// Sanity: the full unfiltered walk would touch `target/` (1 dir) +
	// 200 files = 201 entries, plus `src/` (1 + 5 = 6) and the root
	// listing (3: Dockerfile, .dockerignore, target, src). The post-fix
	// walk should land at the root (1) + Dockerfile/.dockerignore/src
	// listings (3) + src/ (1) + 5 files = ~10, far less than 213.
	reset_walk_counter();
	let _ = build_context_tar(dir.path(), "FROM scratch\nCOPY . /\n", &[])
		.expect("build context must succeed");
	let walked_after_fix = walk_count();
	eprintln!("DEBUG: walked_after_fix={walked_after_fix}");

	// Pre-fix the walk enumerates everything, so the counter is
	// ~213 (every entry in the tree, no pruning). Post-fix the
	// walk prunes `target/` wholesale, so the counter is at most a
	// few dozen. Assert the gap is real and large.
	assert!(
		walked_after_fix < 25,
		"build-context walk enumerated {walked_after_fix} paths; \
		 expected under 25 with `target/` pruned (#1746). The pre-fix \
		 walk would walk at least 201 paths here, one per file under \
		 target/."
	);
	// And the resulting tar is well-formed: it ships the Dockerfile
	// and the src/ files, not anything under target/.
	let tar = build_context_tar(dir.path(), "FROM scratch\nCOPY . /\n", &[])
		.expect("build context must succeed");
	let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(std::io::Cursor::new(tar)));
	let mut paths: Vec<String> = Vec::new();
	for entry in archive.entries().expect("tar entries") {
		let e = entry.expect("entry");
		paths.push(e.path().unwrap().to_string_lossy().into_owned());
	}
	let leaked_target: Vec<&str> = paths
		.iter()
		.filter(|p| p.starts_with("target/") || p.as_str() == "target")
		.map(|p| p.as_str())
		.collect();
	assert!(
		leaked_target.is_empty(),
		"the tar leaked entries under target/ despite the ignore file: {leaked_target:?}"
	);
	// Positive control: the Dockerfile and src/ files are present.
	assert!(paths.iter().any(|p| p == "Dockerfile"));
	assert!(paths.iter().any(|p| p == "src/file0"));
}
