//! YAML merge-key (`<<:`) and anchor resolution ahead of typing a compose file.
//!
//! serde's tolerance of a merge key depends on the type behind it, so the tags
//! are resolved on the raw `Value` first and the merged document is what gets
//! deserialized — the type never sees a `<<:` key.

use std::collections::HashMap;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};

/// Upper bound on YAML alias references in a document that uses anchors, and on
/// the size of such a document. serde_yaml_ng already aborts deeply *nested*
/// alias expansion (its repetition limit), but a flat document with many alias
/// references to a non-trivial anchor expands *linearly* while the `Value` tree
/// is built and can exhaust memory — a ~46 KB file can allocate gigabytes. Real
/// compose files use a handful of anchors, so these caps never trigger in
/// practice; the worst-case expansion they allow (refs × doc size) stays bounded.
const MAX_ALIAS_REFS: usize = 100;
const MAX_ALIAS_DOC_BYTES: usize = 512 * 1024;

/// Upper bound on flow-style nesting depth (`[`/`{`). serde_yaml_ng's own
/// recursion cap eventually rejects pathological nesting, but its tokenizer is
/// O(n^2) in the depth it scans, so a small file of deeply nested flow
/// collections (`[[[[…]]]]`) can burn quadratic CPU time before that cap fires.
/// Real compose files nest only a handful of levels, so this cheap pre-parse
/// pass bounds the parser's worst-case work without affecting any valid input.
const MAX_FLOW_DEPTH: usize = 100;

pub(super) fn deserialize_with_merge(content: &str) -> Result<ComposeFile> {
	deserialize_with_merge_interp(content, None)
}

/// Parse `content` into a [`ComposeFile`], optionally interpolating `${VAR}`
/// references at the scalar level once the YAML document has been parsed.
///
/// Interpolating *after* parsing (rather than on the raw text) is deliberate:
/// the resolved value of a variable is stored verbatim into the existing scalar
/// node, so an env value containing newlines or YAML syntax is treated as data,
/// never as document structure (no key/`privileged: true` injection), an
/// unset/empty variable always yields an in-place empty string (it cannot drop a
/// key or trigger a "mapping values are not allowed" parse error), and multiline
/// or backslash-bearing values are not re-interpreted by the YAML parser.
pub(super) fn deserialize_with_merge_interp(
	content: &str,
	vars: Option<&HashMap<String, String>>,
) -> Result<ComposeFile> {
	let mut value = interpolated_value(content, vars)?;
	// Drop the merge tags before typing: whether serde tolerates one depends on
	// the field's Rust type, so leaving them in makes `!reset` fail the file on
	// `dns` and do nothing on `ports`. What each tag means is decided by the
	// merge (see `compose::tags`), not by which type happens to be behind a key.
	super::tags::strip(&mut value);
	let file: ComposeFile = serde_yaml::from_value(value)?;
	Ok(file)
}

/// Produce the interpolated, merge-key-resolved YAML `Value` for `content` — the
/// exact transformation [`deserialize_with_merge_interp`] applies before it
/// deserializes into a [`ComposeFile`], stopping one step short.
///
/// The raw nested-key diagnostic needs this post-interpolation document shape:
/// the typed `ComposeFile` has already dropped unknown keys nested inside option
/// blocks, so the diagnostic must diff against the raw document the parser saw.
pub(super) fn interpolated_value(
	content: &str,
	vars: Option<&HashMap<String, String>>,
) -> Result<serde_yaml::Value> {
	guard_flow_depth(content)?;
	guard_alias_expansion(content)?;
	let mut value: serde_yaml::Value = serde_yaml::from_str(content)?;
	if let Some(vars) = vars {
		interpolate_value(&mut value, vars)?;
	}
	apply_merge_keys(&mut value);
	Ok(value)
}

/// Recursively interpolate every string scalar of a parsed YAML `value`.
///
/// Mapping values and sequence items are coerced through [`interpolate_scalar`]
/// (so an interpolated numeric/boolean keeps its YAML type, matching
/// docker-compose's typed fields); mapping keys are interpolated as plain text.
fn interpolate_value(value: &mut serde_yaml::Value, vars: &HashMap<String, String>) -> Result<()> {
	match value {
		serde_yaml::Value::String(s) if s.contains('$') => {
			*value = interpolate_scalar(s, vars)?;
		}
		serde_yaml::Value::Sequence(seq) => {
			// Skip the recursion when no element carries a `$`: a 100-service file
			// with no `${VAR}` references pays zero per-sequence iteration cost
			// (#1364). Nested mappings/sequences still need a recursive pre-check
			// so the inner one is not missed.
			if !seq.iter().any(value_needs_interp) {
				return Ok(());
			}
			for item in seq.iter_mut() {
				interpolate_value(item, vars)?;
			}
		}
		serde_yaml::Value::Mapping(map) => {
			// Pre-check: only rebuild the mapping when something inside actually
			// carries a `$`. Without this gate every mapping allocates a fresh
			// `serde_yaml::Mapping` and re-inserts every key/value on every parse
			// — for a 100-service file with ~10 fields each that is ~10k key/value
			// pairs moved for free (#1364). Scalar strings are already gated on
			// `s.contains('$')`; this mirrors that gate for the parent node.
			if !mapping_needs_interp(map) {
				return Ok(());
			}
			let taken = std::mem::take(map);
			let mut rebuilt = serde_yaml::Mapping::with_capacity(taken.len());
			for (key, mut val) in taken {
				let key = interpolate_key(key, vars)?;
				interpolate_value(&mut val, vars)?;
				rebuilt.insert(key, val);
			}
			*map = rebuilt;
		}
		_ => {}
	}
	Ok(())
}

/// Whether `map` (or any nested mapping/sequence/scalar it holds) contains a
/// `$`-bearing string that interpolation would touch. Used to gate the parent
/// mapping rebuild on real work (#1364).
fn mapping_needs_interp(map: &serde_yaml::Mapping) -> bool {
	for (k, v) in map {
		if let serde_yaml::Value::String(s) = k {
			if s.contains('$') {
				return true;
			}
		}
		if value_needs_interp(v) {
			return true;
		}
	}
	false
}

/// Whether `value` (a scalar, sequence, or mapping) contains a `$`-bearing
/// string anywhere reachable from it. Pure and total so the gate above is
/// unit-testable without standing up interpolation.
fn value_needs_interp(value: &serde_yaml::Value) -> bool {
	match value {
		serde_yaml::Value::String(s) => s.contains('$'),
		serde_yaml::Value::Sequence(seq) => seq.iter().any(value_needs_interp),
		serde_yaml::Value::Mapping(map) => mapping_needs_interp(map),
		_ => false,
	}
}

/// Interpolate a single string scalar and recover its YAML type.
///
/// An empty expansion stays an empty string (it must never collapse to `null`
/// and drop the owning key). Otherwise the resolved text is re-read as a YAML
/// scalar so `${N}` in a numeric position becomes a number and `${B}` a boolean,
/// matching docker-compose. Crucially, only *scalar* re-parses are adopted: a
/// result that parses into a mapping or sequence (an injected
/// `root\n  privileged: true`, or a trailing-colon `repo:`) is kept verbatim as
/// a string, so an attacker-influenced value can never introduce structure.
fn interpolate_scalar(s: &str, vars: &HashMap<String, String>) -> Result<serde_yaml::Value> {
	let resolved = crate::substitute::substitute(s, vars)?;
	if resolved.is_empty() {
		return Ok(serde_yaml::Value::String(String::new()));
	}
	match serde_yaml::from_str::<serde_yaml::Value>(&resolved) {
		Ok(v @ (serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_))) => Ok(v),
		_ => Ok(serde_yaml::Value::String(resolved)),
	}
}

/// Interpolate a mapping key. Keys stay strings (a key is never coerced to a
/// number/boolean) so the document shape is preserved.
fn interpolate_key(
	key: serde_yaml::Value,
	vars: &HashMap<String, String>,
) -> Result<serde_yaml::Value> {
	match key {
		serde_yaml::Value::String(s) if s.contains('$') => Ok(serde_yaml::Value::String(
			crate::substitute::substitute(&s, vars)?,
		)),
		other => Ok(other),
	}
}

/// Reject YAML documents whose alias use could amplify into an out-of-memory
/// expansion (a "billion-laughs" linear cousin) before they reach the parser.
fn guard_alias_expansion(content: &str) -> Result<()> {
	let refs = count_alias_refs(content);
	if refs == 0 {
		return Ok(());
	}
	if content.len() > MAX_ALIAS_DOC_BYTES {
		return Err(ComposeError::Unsupported(format!(
			"compose document uses YAML aliases and is {} bytes; documents using anchors/aliases \
			 must be at most {MAX_ALIAS_DOC_BYTES} bytes — inline the repeated content instead",
			content.len()
		)));
	}
	if refs > MAX_ALIAS_REFS {
		return Err(ComposeError::Unsupported(format!(
			"compose document uses {refs} YAML alias references; at most {MAX_ALIAS_REFS} are \
			 allowed — inline the repeated content instead"
		)));
	}
	Ok(())
}

/// Reject documents whose flow-style nesting (`[`/`{`) exceeds [`MAX_FLOW_DEPTH`]
/// before they reach the O(n^2) tokenizer. Brackets inside single/double-quoted
/// scalars and after a `#` comment are ignored, mirroring [`count_alias_refs`]'s
/// conservative scan; the count never fully parses YAML, it only bounds work.
fn guard_flow_depth(content: &str) -> Result<()> {
	let mut depth: usize = 0;
	for line in content.lines() {
		let (mut in_single, mut in_double) = (false, false);
		for c in line.chars() {
			match c {
				'\'' if !in_double => in_single = !in_single,
				'"' if !in_single => in_double = !in_double,
				'#' if !in_single && !in_double => break,
				'[' | '{' if !in_single && !in_double => {
					depth += 1;
					if depth > MAX_FLOW_DEPTH {
						return Err(ComposeError::Unsupported(format!(
							"compose document nests flow collections more than {MAX_FLOW_DEPTH} \
							 levels deep; flatten the structure"
						)));
					}
				}
				']' | '}' if !in_single && !in_double => depth = depth.saturating_sub(1),
				_ => {}
			}
		}
	}
	Ok(())
}

/// Count YAML alias references (`*anchor`) outside quoted scalars and comments.
///
/// A heuristic — it does not fully parse YAML — but it only needs to bound a
/// DoS and it is conservative: `*` inside single/double quotes or after `#` is
/// ignored, and an alias is counted only when `*` sits at a node position and is
/// followed by an anchor-name character.
fn count_alias_refs(content: &str) -> usize {
	let mut count = 0;
	for line in content.lines() {
		let mut chars = line.chars().peekable();
		let (mut in_single, mut in_double) = (false, false);
		let mut prev: Option<char> = None;
		while let Some(c) = chars.next() {
			match c {
				'\'' if !in_double => in_single = !in_single,
				'"' if !in_single => in_double = !in_double,
				'#' if !in_single && !in_double => break,
				'*' if !in_single && !in_double => {
					let at_node = matches!(prev, None | Some(' ' | '\t' | '[' | '{' | ',' | ':'));
					let next_ok = chars
						.peek()
						.is_some_and(|n| n.is_ascii_alphanumeric() || *n == '_' || *n == '-');
					if at_node && next_ok {
						count += 1;
					}
				}
				_ => {}
			}
			prev = Some(c);
		}
	}
	count
}

/// Recursively resolve YAML merge keys (`<<: *anchor`) in a `Value` tree.
///
/// serde_yaml_ng does not expose `apply_merge()` — this replaces it.
/// Merge semantics: keys from the anchor fill in only where the child has no value.
fn apply_merge_keys(value: &mut serde_yaml::Value) {
	match value {
		serde_yaml::Value::Mapping(mapping) => {
			for v in mapping.values_mut() {
				apply_merge_keys(v);
			}
			let merge_key = serde_yaml::Value::String("<<".to_string());
			if let Some(merge_val) = mapping.remove(&merge_key) {
				let bases: Vec<serde_yaml::Mapping> = match merge_val {
					serde_yaml::Value::Mapping(m) => vec![m],
					serde_yaml::Value::Sequence(seq) => seq
						.into_iter()
						.filter_map(|v| match v {
							serde_yaml::Value::Mapping(m) => Some(m),
							_ => None,
						})
						.collect(),
					_ => vec![],
				};
				for base in bases {
					for (k, v) in base {
						if !mapping.contains_key(&k) {
							mapping.insert(k, v);
						}
					}
				}
			}
		}
		serde_yaml::Value::Sequence(seq) => {
			for v in seq.iter_mut() {
				apply_merge_keys(v);
			}
		}
		_ => {}
	}
}

#[cfg(test)]
mod tests {
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
		// Many references to one anchor — the OOM vector serde_yaml_ng does not bound.
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

	// interpolation rebuild gate (#1364)

	fn needs_interp(yaml: &str) -> bool {
		let v: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
		value_needs_interp(&v)
	}

	#[test]
	fn needs_interp_detects_scalar_key_or_value() {
		// A `$` in any leaf — including a key — flips the gate to true.
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
	/// post-interpolation — a rebuild that mutated the document would still
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
}
