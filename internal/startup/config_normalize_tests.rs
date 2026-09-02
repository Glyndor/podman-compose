use super::*;

#[test]
fn yaml11_bool_detection_is_case_insensitive_and_scoped() {
	for tok in [
		"yes", "Yes", "YES", "no", "on", "OFF", "y", "N", "true", "False",
	] {
		assert!(looks_like_yaml11_bool(tok), "{tok} should match");
	}
	for tok in ["hello", "yess", "0", "onoff", "", "nullish"] {
		assert!(!looks_like_yaml11_bool(tok), "{tok} should not match");
	}
}

#[test]
fn quote_yaml11_booleans_quotes_only_plain_bool_scalars() {
	// `yes`/`off` are quoted; an already-quoted `'null'`, a normal string, a
	// nested key, and a non-bool value are all left exactly as serde_yaml wrote
	// them. A `: ` inside a quoted value must not trip the splitter.
	let input = "environment:\n  FROM_A: yes\n  FROM_B: off\n  FROM_C: 'null'\n  NORMAL: hello\n  COLON: 'a: b'\nports:\n- on\n- '8080:80'\n";
	let out = quote_yaml11_booleans(input);
	assert!(out.contains("FROM_A: 'yes'"), "got: {out}");
	assert!(out.contains("FROM_B: 'off'"), "got: {out}");
	assert!(out.contains("FROM_C: 'null'"), "double-quoting: {out}");
	assert!(out.contains("NORMAL: hello"), "got: {out}");
	assert!(out.contains("COLON: 'a: b'"), "got: {out}");
	assert!(out.contains("- 'on'"), "sequence bool item: {out}");
	assert!(out.contains("- '8080:80'"), "got: {out}");
	// The trailing newline is preserved.
	assert!(out.ends_with('\n'));
}

#[cfg(unix)]
#[test]
fn short_bind_source_resolves_relative_only() {
	let base = Path::new("/home/user/proj");
	// A relative `.`-prefixed source is resolved against the project dir, keeping
	// the target and options intact.
	assert_eq!(
		rewrite_short_bind("./data:/data:ro", base).as_deref(),
		Some("/home/user/proj/data:/data:ro")
	);
	// `..` collapses lexically.
	assert_eq!(
		rewrite_short_bind("../shared:/s", base).as_deref(),
		Some("/home/user/shared:/s")
	);
	// Absolute sources, named volumes, and `~` are left untouched.
	assert!(rewrite_short_bind("/abs:/data", base).is_none());
	assert!(rewrite_short_bind("named:/data", base).is_none());
	assert!(rewrite_short_bind("~/x:/data", base).is_none());
	// A colon-less spec (anonymous volume target) is not a bind.
	assert!(rewrite_short_bind("/data", base).is_none());
}

#[cfg(unix)]
#[test]
fn resolve_bind_sources_rewrites_short_and_long_binds() {
	let mut file = podup::parse_str(
		"services:\n  web:\n    image: nginx\n    volumes:\n      - ./data:/data:ro\n      - type: bind\n        source: ./logs\n        target: /logs\n      - named:/cache\n",
	)
	.unwrap();
	resolve_bind_sources(&mut file, Path::new("/srv/app"));
	let mounts = &file.services["web"].volumes;
	match &mounts[0] {
		VolumeMount::Short(s) => assert_eq!(s, "/srv/app/data:/data:ro"),
		other => panic!("expected short, got {other:?}"),
	}
	match &mounts[1] {
		VolumeMount::Long { source, .. } => {
			assert_eq!(source.as_deref(), Some("/srv/app/logs"))
		}
		other => panic!("expected long bind, got {other:?}"),
	}
	// The named volume is untouched.
	match &mounts[2] {
		VolumeMount::Short(s) => assert_eq!(s, "named:/cache"),
		other => panic!("expected short, got {other:?}"),
	}
}
