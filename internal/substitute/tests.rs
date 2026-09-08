use super::*;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
	pairs
		.iter()
		.map(|(k, v)| (k.to_string(), v.to_string()))
		.collect()
}

// Plain passthrough

#[test]
fn plain_text_unchanged() {
	assert_eq!(
		substitute("hello world", &vars(&[])).unwrap(),
		"hello world"
	);
}

#[test]
fn dollar_at_end_emitted_literally() {
	assert_eq!(substitute("price$", &vars(&[])).unwrap(), "price$");
}

#[test]
fn double_dollar_becomes_single() {
	assert_eq!(substitute("$$", &vars(&[])).unwrap(), "$");
}

// $VAR (unbraced)

#[test]
fn unbraced_var_set_expands() {
	assert_eq!(substitute("$FOO", &vars(&[("FOO", "bar")])).unwrap(), "bar");
}

#[test]
fn unbraced_var_unset_expands_to_empty() {
	assert_eq!(substitute("$MISSING", &vars(&[])).unwrap(), "");
}

#[test]
fn unbraced_var_followed_by_non_ident() {
	assert_eq!(substitute("$FOO!", &vars(&[("FOO", "x")])).unwrap(), "x!");
}

// ${VAR} (braced, no modifier)

#[test]
fn braced_var_set_expands() {
	assert_eq!(
		substitute("${FOO}", &vars(&[("FOO", "val")])).unwrap(),
		"val"
	);
}

#[test]
fn braced_var_unset_expands_to_empty() {
	assert_eq!(substitute("${MISSING}", &vars(&[])).unwrap(), "");
}

// ${VAR:-default}

#[test]
fn default_if_unset_or_empty_when_unset() {
	assert_eq!(
		substitute("${X:-fallback}", &vars(&[])).unwrap(),
		"fallback"
	);
}

#[test]
fn default_if_unset_or_empty_when_empty() {
	assert_eq!(
		substitute("${X:-fallback}", &vars(&[("X", "")])).unwrap(),
		"fallback"
	);
}

#[test]
fn default_if_unset_or_empty_when_set() {
	assert_eq!(
		substitute("${X:-fallback}", &vars(&[("X", "real")])).unwrap(),
		"real"
	);
}

// ${VAR-default}

#[test]
fn default_if_unset_when_unset() {
	assert_eq!(substitute("${X-fallback}", &vars(&[])).unwrap(), "fallback");
}

#[test]
fn default_if_unset_when_empty_keeps_empty() {
	assert_eq!(
		substitute("${X-fallback}", &vars(&[("X", "")])).unwrap(),
		""
	);
}

#[test]
fn default_if_unset_when_set() {
	assert_eq!(
		substitute("${X-fallback}", &vars(&[("X", "v")])).unwrap(),
		"v"
	);
}

// ${VAR:+alt}

#[test]
fn alt_if_set_and_nonempty_when_unset() {
	assert_eq!(substitute("${X:+alt}", &vars(&[])).unwrap(), "");
}

#[test]
fn alt_if_set_and_nonempty_when_empty() {
	assert_eq!(substitute("${X:+alt}", &vars(&[("X", "")])).unwrap(), "");
}

#[test]
fn alt_if_set_and_nonempty_when_set() {
	assert_eq!(
		substitute("${X:+alt}", &vars(&[("X", "v")])).unwrap(),
		"alt"
	);
}

// ${VAR+alt}

#[test]
fn alt_if_set_when_unset() {
	assert_eq!(substitute("${X+alt}", &vars(&[])).unwrap(), "");
}

#[test]
fn alt_if_set_when_empty_returns_alt() {
	assert_eq!(substitute("${X+alt}", &vars(&[("X", "")])).unwrap(), "alt");
}

// ${VAR:?msg}

#[test]
fn error_if_unset_or_empty_when_unset() {
	assert!(substitute("${X:?required}", &vars(&[])).is_err());
}

#[test]
fn error_if_unset_or_empty_when_empty() {
	assert!(substitute("${X:?required}", &vars(&[("X", "")])).is_err());
}

#[test]
fn error_if_unset_or_empty_when_set() {
	assert_eq!(
		substitute("${X:?required}", &vars(&[("X", "ok")])).unwrap(),
		"ok"
	);
}

// ${VAR?msg}

#[test]
fn error_if_unset_when_unset() {
	assert!(substitute("${X?required}", &vars(&[])).is_err());
}

#[test]
fn error_if_unset_when_empty_returns_empty() {
	assert_eq!(
		substitute("${X?required}", &vars(&[("X", "")])).unwrap(),
		""
	);
}

// Nested interpolation inside modifier values (compose-spec allows nesting).

#[test]
fn nested_default_is_interpolated() {
	// ${FOO:-${BAR}} → BAR's value when FOO is unset.
	assert_eq!(
		substitute("${FOO:-${BAR}}", &vars(&[("BAR", "b")])).unwrap(),
		"b"
	);
}

#[test]
fn nested_chained_default_falls_through() {
	// ${FOO:-${BAR:-baz}} → literal baz when both FOO and BAR are unset.
	assert_eq!(
		substitute("${FOO:-${BAR:-baz}}", &vars(&[])).unwrap(),
		"baz"
	);
}

#[test]
fn nested_alt_is_interpolated() {
	// ${FOO:+${BAR}} → BAR's value when FOO is set and non-empty.
	assert_eq!(
		substitute("${FOO:+${BAR}}", &vars(&[("FOO", "x"), ("BAR", "b")])).unwrap(),
		"b"
	);
}

#[test]
fn nested_default_with_trailing_text() {
	// The balanced-brace scan stops at the matching close, leaving following text.
	assert_eq!(
		substitute("${FOO:-${BAR}}/tail", &vars(&[("BAR", "b")])).unwrap(),
		"b/tail"
	);
}

// Malformed / pathological references

#[test]
fn empty_name_is_error() {
	assert!(substitute("${}", &vars(&[])).is_err());
}

#[test]
fn digit_leading_name_is_error() {
	assert!(substitute("${1BAD}", &vars(&[])).is_err());
}

#[test]
fn unterminated_modifier_is_error() {
	// The missing `}` must error rather than consume the rest of the input.
	assert!(substitute("${TAG:-latest\nmore", &vars(&[])).is_err());
}

#[test]
fn deeply_nested_defaults_error_instead_of_overflowing() {
	// A pathological `${A:-${A:-…}}` chain (all default branches taken, A unset)
	// returns a clean error past the depth cap rather than overflowing the stack.
	let depth = MAX_INTERP_DEPTH + 50;
	let mut s = String::new();
	for _ in 0..depth {
		s.push_str("${A:-");
	}
	s.push('x');
	for _ in 0..depth {
		s.push('}');
	}
	let err = substitute(&s, &vars(&[])).expect_err("over-deep nesting must error");
	assert!(matches!(
		err,
		crate::error::ComposeError::InvalidSubstitution(_)
	));
}

#[test]
fn moderate_nesting_still_resolves() {
	// Well within the cap, nested defaults resolve normally.
	assert_eq!(
		substitute("${A:-${B:-${C:-deep}}}", &vars(&[])).unwrap(),
		"deep"
	);
}

// Multiple substitutions in one string

#[test]
fn multiple_vars_in_string() {
	let v = vars(&[("A", "hello"), ("B", "world")]);
	assert_eq!(substitute("$A ${B}!", &v).unwrap(), "hello world!");
}

// load_dotenv

#[test]
fn load_dotenv_strips_double_quoted_value() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join(".env"), "FOO=\"bar\"\n").unwrap();
	let map = load_dotenv(dir.path());
	assert_eq!(map.get("FOO").map(|s| s.as_str()), Some("bar"));
}

#[test]
fn load_dotenv_strips_single_quoted_value() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join(".env"), "FOO='bar'\n").unwrap();
	let map = load_dotenv(dir.path());
	assert_eq!(map.get("FOO").map(|s| s.as_str()), Some("bar"));
}

#[test]
fn load_dotenv_parses_key_value() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join(".env"), "FOO=bar\nBAZ=qux\n").unwrap();
	let map = load_dotenv(dir.path());
	assert_eq!(map.get("FOO").map(|s| s.as_str()), Some("bar"));
	assert_eq!(map.get("BAZ").map(|s| s.as_str()), Some("qux"));
}

#[test]
fn load_dotenv_skips_comments_and_blank_lines() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join(".env"), "# comment\n\nFOO=bar\n").unwrap();
	let map = load_dotenv(dir.path());
	assert_eq!(map.len(), 1);
	assert_eq!(map["FOO"], "bar");
}

#[test]
fn load_dotenv_bare_key_is_not_set_to_empty() {
	// A bare key (no `=`) no longer becomes an empty string. In `.env` it
	// resolves from the host, but `load_dotenv` already drops host-present
	// keys (process env wins), so either way a bare key never lands in the map
	// as `""`; it's host-provided for interpolation or absent.
	std::env::set_var("PODUP_DOTENV_ENV_PRESENT", "h");
	std::env::remove_var("PODUP_DOTENV_ENV_ABSENT");
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(
		dir.path().join(".env"),
		"PODUP_DOTENV_ENV_PRESENT\nPODUP_DOTENV_ENV_ABSENT\n",
	)
	.unwrap();
	let map = load_dotenv(dir.path());
	// host-present → dropped (process env wins); host-absent → omitted. Never "".
	assert!(!map.contains_key("PODUP_DOTENV_ENV_PRESENT"));
	assert!(!map.contains_key("PODUP_DOTENV_ENV_ABSENT"));
	std::env::remove_var("PODUP_DOTENV_ENV_PRESENT");
}

#[test]
fn load_dotenv_missing_file_returns_empty() {
	let dir = tempfile::tempdir().unwrap();
	let map = load_dotenv(dir.path());
	assert!(map.is_empty());
}

// build_vars_with_env_files

#[test]
fn env_file_replaces_dotenv() {
	let dir = tempfile::tempdir().unwrap();
	// Compose v2: an explicit `--env-file` replaces the default `.env`, so the
	// dotenv-only key is absent and the extra file's value wins for a shared key.
	std::fs::write(
		dir.path().join(".env"),
		"FROM_DOTENV=base\nPODUP_TEST_SHARED=dotenv\n",
	)
	.unwrap();
	std::fs::write(
		dir.path().join("extra.env"),
		"FROM_EXTRA=more\nPODUP_TEST_SHARED=extra\n",
	)
	.unwrap();

	let vars = build_vars_with_env_files_strict(dir.path(), &["extra.env".to_string()]).unwrap();
	assert_eq!(vars.get("FROM_DOTENV"), None);
	assert_eq!(vars.get("FROM_EXTRA").map(String::as_str), Some("more"));
	assert_eq!(
		vars.get("PODUP_TEST_SHARED").map(String::as_str),
		Some("extra")
	);
}

#[test]
fn later_env_file_wins() {
	let dir = tempfile::tempdir().unwrap();
	// Among several `--env-file`s the last one listed wins.
	std::fs::write(dir.path().join("a.env"), "FROM_A=a\nSHARED=a\n").unwrap();
	std::fs::write(dir.path().join("b.env"), "FROM_B=b\nSHARED=b\n").unwrap();

	let vars =
		build_vars_with_env_files_strict(dir.path(), &["a.env".to_string(), "b.env".to_string()])
			.unwrap();
	assert_eq!(vars.get("FROM_A").map(String::as_str), Some("a"));
	assert_eq!(vars.get("FROM_B").map(String::as_str), Some("b"));
	assert_eq!(vars.get("SHARED").map(String::as_str), Some("b"));
}

#[test]
fn process_env_wins_over_env_file() {
	let dir = tempfile::tempdir().unwrap();
	std::env::set_var("PODUP_ENVFILE_PROCESS_WINS", "from-process");
	std::fs::write(
		dir.path().join("x.env"),
		"PODUP_ENVFILE_PROCESS_WINS=from-file\n",
	)
	.unwrap();

	let vars = build_vars_with_env_files_strict(dir.path(), &["x.env".to_string()]).unwrap();
	assert_eq!(
		vars.get("PODUP_ENVFILE_PROCESS_WINS").map(String::as_str),
		Some("from-process")
	);
	std::env::remove_var("PODUP_ENVFILE_PROCESS_WINS");
}

#[test]
fn no_env_file_loads_dotenv() {
	let dir = tempfile::tempdir().unwrap();
	// With no `--env-file`, `.env` is loaded as before.
	std::fs::write(dir.path().join(".env"), "FROM_DOTENV=base\n").unwrap();
	let vars = build_vars_with_env_files_strict(dir.path(), &[]).unwrap();
	assert_eq!(vars.get("FROM_DOTENV").map(String::as_str), Some("base"));
}

#[test]
fn strict_build_vars_errors_on_missing_extra_file() {
	let dir = tempfile::tempdir().unwrap();
	// An explicitly-passed `--env-file` that does not exist is a hard error
	// (docker compose parity), not a silent fall-back to defaults.
	let err =
		build_vars_with_env_files_strict(dir.path(), &["absent.env".to_string()]).unwrap_err();
	assert!(
		matches!(err, crate::error::ComposeError::EnvFile(_)),
		"expected EnvFile error, got {err:?}"
	);
	assert!(err.to_string().contains("env file not found"));
}

#[test]
fn strict_build_vars_errors_on_unterminated_quote() {
	let dir = tempfile::tempdir().unwrap();
	std::fs::write(dir.path().join("bad.env"), "A=\"oops\nB=keep\n").unwrap();
	let err = build_vars_with_env_files_strict(dir.path(), &["bad.env".to_string()]).unwrap_err();
	assert!(matches!(err, crate::error::ComposeError::EnvFile(_)));
}

#[test]
fn lenient_build_vars_skips_missing_extra_file() {
	let dir = tempfile::tempdir().unwrap();
	// The backward-compatible shim never errors: a missing extra file is
	// silently skipped rather than failing.
	let vars = build_vars_with_env_files(dir.path(), &["absent.env".to_string()]);
	assert!(!vars.contains_key("FROM_EXTRA"));
}

#[test]
fn first_disallowed_control_char_allows_tab_newline_cr() {
	// Legitimate dotenv escapes / multi-line values must pass.
	assert_eq!(first_disallowed_control_char("plain value"), None);
	assert_eq!(first_disallowed_control_char("a\tb\nc\rd"), None);
	// NUL, ESC and other C0/C1 controls are rejected.
	assert_eq!(first_disallowed_control_char("a\0b"), Some('\0'));
	assert_eq!(first_disallowed_control_char("x\x1by"), Some('\x1b'));
}

#[test]
fn strict_build_vars_errors_on_control_char_value_naming_file_and_key() {
	let dir = tempfile::tempdir().unwrap();
	// A NUL in an env-file value is rejected at load time, before any compose
	// parse, with an error that names the originating env file and key, not a
	// misattributed parse offset into the post-substitution document (#885).
	std::fs::write(dir.path().join("bad.env"), "SECRET=ab\0cd\n").unwrap();
	let err = build_vars_with_env_files_strict(dir.path(), &["bad.env".to_string()]).unwrap_err();
	assert!(
		matches!(err, crate::error::ComposeError::EnvFile(_)),
		"expected EnvFile error, got {err:?}"
	);
	let msg = err.to_string();
	assert!(msg.contains("bad.env"), "names the env file: {msg}");
	assert!(msg.contains("SECRET"), "names the offending key: {msg}");
	assert!(
		msg.contains("control"),
		"explains the control-char cause: {msg}"
	);
}

#[test]
fn strict_build_vars_allows_multiline_and_tab_values() {
	let dir = tempfile::tempdir().unwrap();
	// A multi-line quoted value (real newline) and a `\t` escape are legitimate
	// and must not be rejected by the control-char guard.
	std::fs::write(
		dir.path().join("ok.env"),
		"MULTI=\"line one\nline two\"\nTABBED=\"a\\tb\"\n",
	)
	.unwrap();
	let vars = build_vars_with_env_files_strict(dir.path(), &["ok.env".to_string()]).unwrap();
	assert_eq!(
		vars.get("MULTI").map(String::as_str),
		Some("line one\nline two")
	);
	assert_eq!(vars.get("TABBED").map(String::as_str), Some("a\tb"));
}

// variable-repetition cap (#1738)

#[test]
fn interpolation_cap_refuses_variable_repetition_in_one_scalar() {
	// 300 KB of `${PAYLOAD}` references in one scalar with a moderately sized
	// PAYLOAD value would otherwise produce hundreds of MB of output and
	// (on the host from the issue) SIGABRT in under a second. The cap is
	// checked as `out` grows inside `substitute_depth`, so the refusal
	// fires before the offending allocation is committed, and names both
	// the variable and the size so a legitimate large value can be told
	// apart from a hostile one (#1738).
	let payload = "x".repeat(2 * 1024);
	let mut input = String::new();
	while input.len() < 300 * 1024 {
		input.push_str("${PAYLOAD}");
	}
	let v = vars(&[("PAYLOAD", &payload)]);
	let err = substitute(&input, &v)
		.expect_err("expected the interpolation cap to refuse variable repetition");
	let msg = format!("{err}");
	assert!(
		matches!(err, crate::error::ComposeError::InvalidSubstitution(_)),
		"refusal should be InvalidSubstitution; got: {msg}"
	);
	assert!(
		msg.contains("PAYLOAD"),
		"refusal should name the variable; got: {msg}"
	);
	assert!(
		msg.contains("bytes"),
		"refusal should name the size reached; got: {msg}"
	);
}

#[test]
fn interpolation_cap_allows_legitimate_repetition() {
	// A handful of repetitions of a small value stay well under the cap.
	let v = vars(&[("TAG", "v1")]);
	assert_eq!(substitute("${TAG}-${TAG}-${TAG}", &v).unwrap(), "v1-v1-v1");
}

#[test]
fn the_document_budget_bounds_repetition_spread_across_scalars() {
	// The per-scalar cap is the wrong unit on its own: repetition split
	// across scalars is unbounded while every single substitution stays
	// legal. Measured before this budget existed, with a 1 MiB value and
	// one service reading it from N environment entries: 256 reached
	// 1.6 GB, 512 reached 3.2 GB, 1024 aborted at 5.27 GB, and no single
	// substitution passed 1 MiB, so the per-scalar cap never fired.
	//
	// The value is built here rather than committed as a fixture.
	let big = "y".repeat(1024 * 1024);
	let mut vars = std::collections::HashMap::new();
	vars.insert("BIG".to_string(), big);

	// Under the budget: still accepted, because a long secret across a
	// handful of fields is a legitimate document.
	let mut spent = 0usize;
	for _ in 0..8 {
		super::substitute_budgeted("${BIG}", &vars, &mut spent)
			.expect("eight one-mebibyte substitutions stay under the budget");
	}

	// Over it: refused, and the message names the variable so a large
	// legitimate file can be told apart from a hostile one.
	let mut spent = 0usize;
	let err = (0..64)
		.map(|_| super::substitute_budgeted("${BIG}", &vars, &mut spent))
		.find_map(|r| r.err())
		.expect("sixty-four must cross the document budget");
	let msg = format!("{err}");
	assert!(
		msg.contains("BIG") && msg.contains("past"),
		"the refusal must name the variable and the budget, got: {msg}"
	);
}
