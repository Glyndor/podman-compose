//! `${VAR}` / `$VAR` reference parsing and modifier resolution.
//!
//! Implements the compose-spec modifier forms (`:-`, `-`, `:+`, `+`, `:?`, `?`)
//! by scanning the characters inside a `${…}` group and resolving them against
//! the variable map.

use std::collections::HashMap;

use crate::error::{ComposeError, Result};

pub(super) fn is_var_start(c: char) -> bool {
	c.is_alphabetic() || c == '_'
}

fn is_var_char(c: char) -> bool {
	c.is_alphanumeric() || c == '_'
}

pub(super) fn collect_var_name(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
	let mut name = String::new();
	while let Some(&c) = chars.peek() {
		if is_var_char(c) {
			name.push(c);
			chars.next();
		} else {
			break;
		}
	}
	name
}

#[derive(Debug)]
pub(super) enum Modifier {
	None,
	/// `${VAR:-default}`: use default if unset or empty
	DefaultIfUnsetOrEmpty(String),
	/// `${VAR-default}`: use default if unset (empty value is OK)
	DefaultIfUnset(String),
	/// `${VAR:+value}`: use value if set and non-empty
	AltIfSetAndNonEmpty(String),
	/// `${VAR+value}`: use value if set (even if empty)
	AltIfSet(String),
	/// `${VAR:?error}`: error if unset or empty
	ErrorIfUnsetOrEmpty(String),
	/// `${VAR?error}`: error if unset
	ErrorIfUnset(String),
}

/// Parse the content inside `${…}`.  The opening `{` has already been consumed.
///
/// The variable name is collected with [`is_var_char`] (matching the unbraced
/// `$VAR` path and the compose-spec grammar). It must be non-empty and start
/// with `[A-Za-z_]`; `${}` and `${1BAD}` are rejected as malformed rather than
/// resolved to an empty string. After the name, only a modifier delimiter
/// (`}`, `:`, `-`, `+`, `?`) or end-of-input may follow; any other trailing
/// character (a space in `${FOO BAR}`, a dot in `${FOO.BAR}`, …) makes the
/// reference malformed and is rejected rather than folded into the lookup key.
pub(super) fn parse_braced_var(
	chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
) -> Result<(String, Modifier)> {
	let name = collect_var_name(chars);

	// The name must be a valid identifier: non-empty and starting with a letter
	// or `_`. `collect_var_name` accepts digits (they are valid *within* a name),
	// so an empty name or a digit-leading name only fails here.
	if name.is_empty() || !name.starts_with(is_var_start) {
		return Err(ComposeError::InvalidSubstitution(format!(
			"invalid variable name {name:?} in '${{…}}': names must start with a letter or '_'"
		)));
	}

	match chars.peek() {
		None => Ok((name, Modifier::None)),
		Some('}') => {
			chars.next();
			Ok((name, Modifier::None))
		}
		Some(':') => {
			chars.next();
			// Peek at what follows `:`.
			let modifier = match chars.peek() {
				Some('-') => {
					chars.next();
					Modifier::DefaultIfUnsetOrEmpty(collect_until_close(chars)?)
				}
				Some('+') => {
					chars.next();
					Modifier::AltIfSetAndNonEmpty(collect_until_close(chars)?)
				}
				Some('?') => {
					chars.next();
					Modifier::ErrorIfUnsetOrEmpty(collect_until_close(chars)?)
				}
				_ => Modifier::DefaultIfUnsetOrEmpty(collect_until_close(chars)?),
			};
			Ok((name, modifier))
		}
		Some('-') => {
			chars.next();
			Ok((name, Modifier::DefaultIfUnset(collect_until_close(chars)?)))
		}
		Some('+') => {
			chars.next();
			Ok((name, Modifier::AltIfSet(collect_until_close(chars)?)))
		}
		Some('?') => {
			chars.next();
			Ok((name, Modifier::ErrorIfUnset(collect_until_close(chars)?)))
		}
		Some(&c) => Err(ComposeError::InvalidSubstitution(format!(
			"unexpected character {c:?} in variable name '${{{name}…}}'"
		))),
	}
}

/// Collect the modifier value up to the matching closing `}` (consumed),
/// balancing nested braces so an inner `${…}` is captured whole. For
/// `${FOO:-${BAR}}` the default is `${BAR}` (not `${BAR`), enabling nested
/// interpolation in [`resolve_modifier`].
///
/// If the input ends before the matching `}` is reached the reference is
/// unterminated (e.g. `${TAG:-latest` with no closing brace), which would
/// otherwise silently swallow the rest of the document as the modifier value;
/// that is reported as [`ComposeError::InvalidSubstitution`] instead.
fn collect_until_close(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String> {
	let mut buf = String::new();
	let mut depth = 0u32;
	for c in chars.by_ref() {
		match c {
			'{' => {
				depth += 1;
				buf.push(c);
			}
			'}' if depth == 0 => return Ok(buf),
			'}' => {
				depth -= 1;
				buf.push(c);
			}
			_ => buf.push(c),
		}
	}
	Err(ComposeError::InvalidSubstitution(
		"unterminated variable substitution: missing closing '}'".to_string(),
	))
}

/// Apply a parsed `Modifier` to `var`, implementing compose's
/// `${VAR:-default}` / `${VAR-default}` / `${VAR:+alt}` / `${VAR+alt}` /
/// `${VAR:?err}` / `${VAR?err}` substitution semantics (the `:` forms treat an
/// empty value like unset). `Modifier::None` returns the value or an empty
/// string; the `Error*` variants fail when the condition is unmet.
pub(super) fn resolve_modifier(
	var: String,
	modifier: Modifier,
	vars: &HashMap<String, String>,
	depth: usize,
) -> Result<String> {
	let value = vars.get(&var);

	match modifier {
		Modifier::None => match value {
			Some(v) => Ok(v.clone()),
			None => {
				// Match docker compose v2, which warns on stderr before defaulting an
				// unreferenced variable to the empty string, so config typos surface.
				tracing::warn!("The {var} variable is not set. Defaulting to a blank string.");
				Ok(String::new())
			}
		},

		// Default/alt values are themselves interpolated (compose allows nesting,
		// e.g. `${FOO:-${BAR}}`), but only when actually used. `depth` bounds that
		// recursion so a pathological `${A:-${A:-…}}` chain returns an error rather
		// than overflowing the stack.
		Modifier::DefaultIfUnsetOrEmpty(default) => match value {
			Some(v) if !v.is_empty() => Ok(v.clone()),
			_ => super::substitute_depth(&default, vars, depth + 1),
		},

		Modifier::DefaultIfUnset(default) => match value {
			Some(v) => Ok(v.clone()),
			None => super::substitute_depth(&default, vars, depth + 1),
		},

		Modifier::AltIfSetAndNonEmpty(alt) => match value {
			Some(v) if !v.is_empty() => super::substitute_depth(&alt, vars, depth + 1),
			_ => Ok(String::new()),
		},

		Modifier::AltIfSet(alt) => match value {
			Some(_) => super::substitute_depth(&alt, vars, depth + 1),
			None => Ok(String::new()),
		},

		Modifier::ErrorIfUnsetOrEmpty(msg) => match value {
			Some(v) if !v.is_empty() => Ok(v.clone()),
			_ => Err(ComposeError::RequiredVarNotSet { var, msg }),
		},

		Modifier::ErrorIfUnset(msg) => match value {
			Some(v) => Ok(v.clone()),
			None => Err(ComposeError::RequiredVarNotSet { var, msg }),
		},
	}
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
