use super::abs_context;
use std::path::Path;

#[test]
fn abs_context_makes_relative_build_contexts_absolute() {
	let base = Path::new("/srv/app");
	// `.` is the compose file's own directory.
	assert_eq!(abs_context(base, "."), "/srv/app");
	// A `./`-prefixed or bare relative path joins under the base, kept clean.
	assert_eq!(abs_context(base, "./src"), "/srv/app/src");
	assert_eq!(abs_context(base, "src"), "/srv/app/src");
	// A parent traversal is preserved (systemd/podman resolve it).
	assert_eq!(abs_context(base, "../shared"), "/srv/app/../shared");
	// An already-absolute context is passed through untouched.
	assert_eq!(abs_context(base, "/opt/build"), "/opt/build");
}
