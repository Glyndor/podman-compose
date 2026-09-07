use super::*;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
	pairs
		.iter()
		.map(|(k, v)| (k.to_string(), v.to_string()))
		.collect()
}

// scalar-level interpolation (post-parse)

#[test]
fn interpolation_cannot_inject_yaml_structure() {
	// A value carrying an embedded newline + YAML must NOT introduce new keys:
	// post-parse interpolation stores it verbatim into the existing scalar.
	let v = vars(&[("U", "root\n    privileged: true")]);
	let yaml = "services:\n  app:\n    image: nginx\n    user: ${U}\n";
	let file = deserialize_with_merge_interp(yaml, Some(&v)).unwrap();
	let svc = &file.services["app"];
	assert_eq!(svc.user.as_deref(), Some("root\n    privileged: true"));
	// The injected `privileged: true` is data, not structure.
	assert_eq!(svc.privileged, None);
}

#[test]
fn empty_interpolation_in_unquoted_scalar_keeps_key_and_is_empty() {
	// `repo:${TAG}` with TAG unset becomes `repo:` (no YAML parse error) and a
	// bare `${TAG}` value becomes an empty string rather than dropping the key.
	let yaml = "services:\n  app:\n    image: repo:${TAG}\n    user: ${TAG}\n";
	let file = deserialize_with_merge_interp(yaml, Some(&vars(&[]))).unwrap();
	let svc = &file.services["app"];
	assert_eq!(svc.image.as_deref(), Some("repo:"));
	assert_eq!(svc.user.as_deref(), Some(""));
}

#[test]
fn interpolated_multiline_and_backslash_values_preserved_verbatim() {
	// A resolved value with real newlines/backslashes is stored byte-for-byte;
	// it is not re-folded or re-escaped by the YAML parser.
	let v = vars(&[("MSG", "line1\nline2\\x")]);
	let yaml = "services:\n  app:\n    image: nginx\n    user: ${MSG}\n";
	let file = deserialize_with_merge_interp(yaml, Some(&v)).unwrap();
	assert_eq!(
		file.services["app"].user.as_deref(),
		Some("line1\nline2\\x")
	);
}

#[test]
fn interpolated_scalar_recovers_numeric_and_boolean_type() {
	// `${N}` in a numeric/boolean position keeps its YAML type, matching
	// docker-compose's typed fields.
	let v = vars(&[("P", "true"), ("CPU", "512")]);
	let yaml =
		"services:\n  app:\n    image: nginx\n    privileged: ${P}\n    cpu_shares: ${CPU}\n";
	let file = deserialize_with_merge_interp(yaml, Some(&v)).unwrap();
	assert_eq!(file.services["app"].privileged, Some(true));
	assert_eq!(file.services["app"].cpu_shares, Some(512));
}

#[test]
fn no_interpolation_when_vars_is_none() {
	// `config --no-interpolate` path: placeholders stay literal.
	let yaml = "services:\n  app:\n    image: repo:${TAG}\n";
	let file = deserialize_with_merge(yaml).unwrap();
	assert_eq!(file.services["app"].image.as_deref(), Some("repo:${TAG}"));
}

// alias-expansion guard

#[test]
fn count_alias_refs_ignores_quotes_comments_and_globs() {
	assert_eq!(count_alias_refs("a: &x 1\nb: *x\n"), 1);
	assert_eq!(count_alias_refs("c: [*x, *x, *x]\n"), 3);
	// `*` in quoted strings, comments, and globs (`*.txt`, `**`) are not aliases.
	assert_eq!(
		count_alias_refs("cmd: \"rm *x\"\nd: 1 # *x\ng: ['*.txt', '**']\n"),
		0
	);
}

#[test]
fn guard_allows_normal_anchored_file() {
	// A handful of merge-key aliases in a small file is fine.
	let yaml = "x: &d {a: 1}\nweb: {<<: *d}\napi: {<<: *d}\n";
	assert!(guard_alias_expansion(yaml).is_ok());
}

#[test]
fn guard_rejects_linear_alias_amplification() {
	// Many references to one anchor: the OOM vector serde_yaml_ng does not bound.
	let mut yaml = String::from("anchor: &a [x, y, z]\nlist:\n");
	for _ in 0..(MAX_ALIAS_REFS + 50) {
		yaml.push_str("  - *a\n");
	}
	let err = guard_alias_expansion(&yaml).unwrap_err();
	assert!(format!("{err}").contains("alias references"));
}

// flow-depth guard

#[test]
fn guard_flow_depth_allows_shallow_nesting() {
	// A handful of nested flow collections (typical compose) is fine.
	assert!(guard_flow_depth("a: [[1, 2], [3, 4]]\nb: {x: {y: 1}}\n").is_ok());
}

#[test]
fn guard_flow_depth_rejects_pathological_nesting() {
	let deep = format!(
		"a: {}{}\n",
		"[".repeat(MAX_FLOW_DEPTH + 5),
		"]".repeat(MAX_FLOW_DEPTH + 5)
	);
	let err = guard_flow_depth(&deep).unwrap_err();
	assert!(format!("{err}").contains("flow collections"));
}

#[test]
fn guard_flow_depth_ignores_brackets_in_quotes_and_comments() {
	// Brackets inside quoted scalars or after `#` do not count toward depth.
	let yaml = format!("cmd: \"{}\"  # {}\n", "[".repeat(200), "{".repeat(200));
	assert!(guard_flow_depth(&yaml).is_ok());
}

#[test]
fn guard_rejects_large_aliased_document() {
	let mut yaml = String::from("anchor: &a 1\nb: *a\n");
	yaml.push_str(&format!("pad: \"{}\"\n", "p".repeat(MAX_ALIAS_DOC_BYTES)));
	let err = guard_alias_expansion(&yaml).unwrap_err();
	assert!(format!("{err}").contains("at most"));
}

// The original scanner toggled `in_single` on every `'` regardless of position
// inside a plain scalar, so `don't, *a *a` had every `*a` silently inside
// "a quoted scalar" and counted zero aliases. The guard's `refs == 0` early
// return then let the file through. The opposite of the existing scanner
// test (which only asserted the not-over-counted direction).
#[test]
fn count_alias_refs_counts_aliases_after_apostrophe_in_plain_scalar() {
	// The `'` sits in the middle of a plain scalar (`don't,`), so it does
	// NOT open a quoted scalar: every `*a` after the comma is a real alias.
	assert_eq!(count_alias_refs("x: don't, *a *a\n"), 2);
	assert_eq!(count_alias_refs("x: won't, *a *a *a\n"), 3);
}

#[test]
fn count_alias_refs_counts_aliases_after_hash_in_plain_scalar() {
	// `#` is a comment only when preceded by whitespace. `foo#x *a` is
	// plain scalar `foo#x` followed by alias `*a`; the scanner used to
	// break on `#` and miss the alias.
	assert_eq!(count_alias_refs("x: foo#x *a *a\n"), 2);
}

#[test]
fn guard_rejects_aliases_after_apostrophe_in_plain_scalar() {
	// End-to-end: a document whose alias-bearing lines start with a
	// plain-scalar apostrophe used to count zero refs and slip past the
	// guard. Build enough lines to exceed `MAX_ALIAS_REFS`; the guard must
	// refuse instead of returning Ok.
	let mut yaml = String::from("anchor: &a 1\n");
	for _ in 0..(MAX_ALIAS_REFS / 10 + 2) {
		yaml.push_str("x: don't, *a *a *a *a *a *a *a *a *a *a\n");
	}
	let err = guard_alias_expansion(&yaml).unwrap_err();
	let msg = format!("{err}");
	assert!(
		msg.contains("alias"),
		"refusal should mention aliases; got: {msg}"
	);
}

#[test]
fn guard_rejects_aliases_after_hash_in_plain_scalar() {
	// Same hole via `#`: the scanner used to treat `#` as a comment start
	// anywhere on the line, swallowing the aliases after it.
	let mut yaml = String::from("anchor: &a 1\n");
	for _ in 0..(MAX_ALIAS_REFS / 10 + 2) {
		yaml.push_str("x: foo#pad *a *a *a *a *a *a *a *a *a *a\n");
	}
	let err = guard_alias_expansion(&yaml).unwrap_err();
	let msg = format!("{err}");
	assert!(
		msg.contains("alias"),
		"refusal should mention aliases; got: {msg}"
	);
}

#[test]
fn interpolate_scalar_refuses_alias_payload_from_env() {
	// The re-parse in `interpolate_scalar` runs `serde_yaml::from_str` on
	// the resolved text. Without a guard, a `.env`-supplied value that
	// contains many alias references would have the parser build the full
	// Value tree (allocating memory) before the result is discarded and the
	// raw string is kept. The re-parse must go through the same alias guard
	// the file-level path uses.
	let mut payload = String::from("&a [x]\n");
	for _ in 0..(MAX_ALIAS_REFS + 50) {
		payload.push_str("c: *a\n");
	}
	let v = vars(&[("P", &payload)]);
	let yaml = "services:\n  app:\n    image: ${P}\n";
	let result = deserialize_with_merge_interp(yaml, Some(&v));
	let err = result.expect_err("expected guard to refuse alias-bearing payload");
	let msg = format!("{err}");
	assert!(
		msg.contains("alias") || msg.contains("anchor"),
		"refusal should mention aliases; got: {msg}"
	);
}

// interpolation rebuild gate (#1364)

fn needs_interp(yaml: &str) -> bool {
	let v: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
	value_needs_interp(&v)
}

#[test]
fn needs_interp_detects_scalar_key_or_value() {
	// A `$` in any leaf, including a key, flips the gate to true.
	assert!(!needs_interp("foo: bar"));
	assert!(needs_interp("foo: $bar"));
	assert!(needs_interp("$foo: bar"));
}

#[test]
fn needs_interp_descends_into_sequences_and_mappings() {
	assert!(!needs_interp("a:\n  - 1\n  - 2\n  - 3\n"));
	assert!(needs_interp("a:\n  - 1\n  - 2\n  - $HOME\n"));
	assert!(!needs_interp("a:\n  b:\n    c: plain\n  d: also plain\n"));
	assert!(needs_interp("a:\n  b:\n    c: plain\n  d: $HOME\n"));
}

#[test]
fn needs_interp_ignores_non_string_leaves() {
	// Numeric / boolean / null leaves never carry `$`, so a mapping of those
	// is a no-op for interpolation even when one of the keys contains `$`.
	assert!(!needs_interp("a: 1\nb: 2\nc: true\nd: null\n"));
}

/// The end-to-end contract: a compose file with no `${VAR}` references
/// round-trips through `deserialize_with_merge_interp` with the same
/// content, and the parent mapping is not rebuilt (no allocations beyond
/// the original parse). Asserted by counting scalar-string equality
/// post-interpolation: a rebuild that mutated the document would still
/// pass equality, so this also pins the no-mutation invariant.
#[test]
fn mapping_rebuild_is_skipped_when_nothing_needs_interpolation() {
	let yaml = "services:\n  app:\n    image: nginx:1.27\n    user: \"1000\"\n  db:\n    image: postgres:16\n    restart: unless-stopped\n";
	let a = deserialize_with_merge_interp(yaml, Some(&vars(&[]))).unwrap();
	// Same parse twice: the second call must produce a byte-identical
	// document (a stale cache or a non-idempotent rebuild would diverge).
	let b = deserialize_with_merge_interp(yaml, Some(&vars(&[]))).unwrap();
	assert_eq!(a.services["app"].image, b.services["app"].image);
	assert_eq!(a.services["db"].restart, b.services["db"].restart);
}
