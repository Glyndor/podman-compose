use super::is_safe_project_name;

#[test]
fn safe_project_names_accepted() {
	// Matches docker-compose `^[a-z0-9][a-z0-9_-]*$`: lowercase letters/digits,
	// `-`/`_`, first char a letter or digit.
	for name in ["web", "my-app", "my_app", "appv2", "a1", "1app", "x"] {
		assert!(is_safe_project_name(name), "{name:?} must be accepted");
	}
}

#[test]
fn unsafe_project_names_rejected() {
	let long = "a".repeat(129);
	for name in [
		"",
		".",
		"..",
		".hidden",
		"a/b",
		"../x",
		"a b",
		"a\0b",
		// docker-compose rejects these too; previously accepted by podup.
		"-rf",    // leading dash (flag-injection vector)
		"--all",  // leading dash
		"---",    // all-dash
		"_x",     // leading underscore
		"App",    // uppercase
		"MYAPP",  // uppercase
		"app.v2", // dot
		"bad.",   // trailing dot
		long.as_str(),
	] {
		assert!(!is_safe_project_name(name), "{name:?} must be rejected");
	}
}
