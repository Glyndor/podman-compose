//! Minimal dotenv parser shared by `env_file` loading and `.env` interpolation.
//!
//! Implements the subset of compose-spec dotenv rules podup relies on:
//! full-line comments, an optional `export` prefix, single- and double-quoted
//! values (including values that span multiple lines), inline comments on
//! unquoted values, and the standard double-quote escape sequences. Callers
//! decide duplicate-key precedence by the order pairs are returned in.

/// Parse dotenv `content` into ordered `(key, value)` pairs (lenient).
///
/// Pairs are returned in file order; a later duplicate key appears after an
/// earlier one, leaving the precedence decision to the caller. This variant is
/// used for the optional default `.env`: it never fails, so an unterminated
/// quoted value degrades to consuming the rest of the file (historical
/// behaviour) rather than erroring.
pub fn parse(content: &str) -> Vec<(String, String)> {
	// `strict = false` can never produce an error.
	parse_inner(content, false).unwrap_or_default()
}

/// Like [`parse`] but rejects malformed input.
///
/// Used for explicitly requested env files (`--env-file`, a service `env_file:`)
/// where a typo'd or truncated file must fail loudly rather than silently drop
/// variables — matching docker compose, which hard-errors on an unterminated
/// quoted value.
pub fn parse_strict(content: &str) -> crate::error::Result<Vec<(String, String)>> {
	parse_inner(content, true)
}

fn parse_inner(content: &str, strict: bool) -> crate::error::Result<Vec<(String, String)>> {
	// Strip a leading UTF-8 BOM so the first key is not captured as
	// `\u{feff}KEY` (which would silently lose that variable). Matches
	// docker/godotenv, which drop a leading BOM before parsing.
	let content = content.strip_prefix('\u{feff}').unwrap_or(content);

	let mut out = Vec::new();
	let mut lines = content.lines();

	while let Some(raw) = lines.next() {
		let line = raw.trim_start();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let line = line
			.strip_prefix("export ")
			.map(str::trim_start)
			.unwrap_or(line);

		let Some(eq) = line.find('=') else {
			// A bare key (no `=`) means "pass the value through from the host
			// environment" (compose env_file semantics). If the host doesn't
			// define it, the variable is omitted, not set to an empty string.
			let key = line.trim();
			if !key.is_empty() {
				if let Ok(val) = std::env::var(key) {
					out.push((key.to_string(), val));
				}
			}
			continue;
		};

		let key = line[..eq].trim();
		if key.is_empty() {
			continue;
		}
		let value = parse_value(line[eq + 1..].trim_start(), &mut lines, key, strict)?;
		out.push((key.to_string(), value));
	}

	Ok(out)
}

/// Parse the value portion of an assignment, consuming continuation lines for
/// a quoted value that does not close on the first line. In `strict` mode a
/// quote that never closes is a hard error instead of swallowing the rest of
/// the file (which would silently drop every following key).
fn parse_value(
	rest: &str,
	lines: &mut std::str::Lines,
	key: &str,
	strict: bool,
) -> crate::error::Result<String> {
	match rest.chars().next() {
		Some(quote @ ('"' | '\'')) => {
			let body = &rest[quote.len_utf8()..];
			if let Some(end) = find_closing(body, quote) {
				return Ok(unescape(&body[..end], quote));
			}
			// Unterminated on this line: a multi-line quoted value.
			let mut buf = String::from(body);
			for next in lines.by_ref() {
				buf.push('\n');
				if let Some(end) = find_closing(next, quote) {
					buf.push_str(&next[..end]);
					return Ok(unescape(&buf, quote));
				}
				buf.push_str(next);
			}
			if strict {
				return Err(crate::error::ComposeError::EnvFile(format!(
					"unterminated quoted value for key '{key}'"
				)));
			}
			Ok(unescape(&buf, quote))
		}
		_ => Ok(strip_inline_comment(rest).trim_end().to_string()),
	}
}

/// Byte index of the unescaped closing `quote` in `s`, if present.
///
/// Backslash escapes are honoured for double quotes only; single-quoted
/// strings are literal and close at the first quote.
fn find_closing(s: &str, quote: char) -> Option<usize> {
	let bytes = s.as_bytes();
	let q = quote as u8;
	let mut i = 0;
	while i < bytes.len() {
		let c = bytes[i];
		if quote == '"' && c == b'\\' {
			i += 2;
			continue;
		}
		if c == q {
			return Some(i);
		}
		i += 1;
	}
	None
}

/// Expand escape sequences for double-quoted values; single-quoted values are
/// returned verbatim.
fn unescape(s: &str, quote: char) -> String {
	if quote == '\'' {
		return s.to_string();
	}
	let mut out = String::with_capacity(s.len());
	let mut chars = s.chars();
	while let Some(c) = chars.next() {
		if c != '\\' {
			out.push(c);
			continue;
		}
		match chars.next() {
			Some('n') => out.push('\n'),
			Some('r') => out.push('\r'),
			Some('t') => out.push('\t'),
			Some('\\') => out.push('\\'),
			Some('"') => out.push('"'),
			Some('\'') => out.push('\''),
			Some(other) => out.push(other),
			None => out.push('\\'),
		}
	}
	out
}

/// Trim an inline comment from an unquoted value: everything from the first
/// `#` that is preceded by whitespace. A `#` with no preceding whitespace
/// (e.g. directly after `=`) is part of the value.
fn strip_inline_comment(s: &str) -> &str {
	let bytes = s.as_bytes();
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'#' && i > 0 && bytes[i - 1].is_ascii_whitespace() {
			return &s[..i];
		}
		i += 1;
	}
	s
}

#[cfg(test)]
#[path = "dotenv_tests.rs"]
mod tests;
