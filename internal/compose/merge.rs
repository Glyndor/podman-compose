//! YAML merge-key (`<<:`) and anchor resolution ahead of typing a compose file.
//!
//! serde's tolerance of a merge key depends on the type behind it, so the tags
//! are resolved on the raw `Value` first and the merged document is what gets
//! deserialized; the type never sees a `<<:` key.

use std::collections::HashMap;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};

/// Upper bound on YAML alias references in a document that uses anchors, and on
/// the size of such a document. serde_yaml_ng already aborts deeply *nested*
/// alias expansion (its repetition limit), but a flat document with many alias
/// references to a non-trivial anchor expands *linearly* while the `Value` tree
/// is built and can exhaust memory: a ~46 KB file can allocate gigabytes. Real
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

/// Produce the interpolated, merge-key-resolved YAML `Value` for `content`, the
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
			// For a 100-service file with ~10 fields each that is ~10k key/value
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
			 must be at most {MAX_ALIAS_DOC_BYTES} bytes; inline the repeated content instead",
			content.len()
		)));
	}
	if refs > MAX_ALIAS_REFS {
		return Err(ComposeError::Unsupported(format!(
			"compose document uses {refs} YAML alias references; at most {MAX_ALIAS_REFS} are \
			 allowed; inline the repeated content instead"
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
/// A heuristic (it does not fully parse YAML) but it only needs to bound a
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
/// serde_yaml_ng does not expose `apply_merge()`, so this replaces it.
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
#[path = "merge_tests.rs"]
mod tests;
