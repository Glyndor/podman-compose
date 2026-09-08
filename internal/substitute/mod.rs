//! Docker Compose variable substitution.
//!
//! Applies `${VAR}` / `$VAR` substitution to individual scalar values of a
//! parsed compose document (compose-spec value-level interpolation).
//! Handles all compose-spec modifier forms: `:-`, `-`, `:+`, `+`, `:?`, `?`.

mod parse;

use std::collections::HashMap;
use std::path::Path;

use crate::error::{ComposeError, Result};

use parse::{collect_var_name, is_var_start, parse_braced_var, resolve_modifier};

/// Maximum nesting depth for interpolated default/alternate values
/// (`${A:-${A:-…}}`). Real compose files nest a handful of levels at most; this
/// cap turns a pathological chain into a clean error instead of a stack overflow.
const MAX_INTERP_DEPTH: usize = 64;

/// Upper bound on the cumulative size of the output of a single [`substitute`]
/// pass. The output grows by `count(refs_in_input) × len(value_of_VAR)`, with
/// no natural ceiling: a few hundred kilobytes of input where one variable
/// repeats many times reaches gigabytes before anything else has a chance to
/// fire (#1738). The bound is checked as `out` grows (before each `push_str`)
/// so the refusal happens before the offending allocation, and the message
/// names the variable and the size so a legitimate large payload (a long
/// secret, a multi-line config blob) can be told apart from a hostile one.
const MAX_INTERP_OUTPUT_BYTES: usize = 4 * 1024 * 1024;

/// Upper bound on the total interpolated output of one document.
///
/// [`MAX_INTERP_OUTPUT_BYTES`] bounds a single scalar, which is the wrong
/// unit on its own: repetition split across scalars is unbounded while every
/// individual substitution stays legal. Measured on the tree before this
/// bound existed, with a 1 MiB `.env` value and one service whose
/// environment carries N entries all reading it: 256 entries reached 1.6 GB,
/// 512 reached 3.2 GB, and 1024 aborted at 5.27 GB. No single substitution
/// exceeded 1 MiB, so the per-scalar cap never fired once.
///
/// 16 MiB, the same number the per-file cap already uses, so a reader meets
/// one size rather than two. It was picked by measurement rather than taste:
/// at 64 MiB a document of 64 one-mebibyte entries is accepted and costs
/// 419 MB resident, which is not a bound worth having. At 16 MiB the same
/// shape is refused and a document that is legitimately large, a long secret
/// or a config blob across a handful of services, still parses.
///
/// The per-scalar cap stays as the cheaper first line: it refuses a single
/// runaway value without walking the rest of the document.
pub(crate) const MAX_INTERP_DOCUMENT_BYTES: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Substitute all `$VAR` / `${VAR}` references in `input` using `vars`.
///
/// `vars` should contain both the process environment and the `.env` file
/// entries (process environment takes precedence).
pub fn substitute(input: &str, vars: &HashMap<String, String>) -> Result<String> {
	let mut spent = 0usize;
	substitute_depth(input, vars, 0, &mut spent)
}

/// [`substitute`] with a budget that outlives the call, so a caller
/// interpolating many scalars bounds their total rather than each one.
pub(crate) fn substitute_budgeted(
	input: &str,
	vars: &HashMap<String, String>,
	spent: &mut usize,
) -> Result<String> {
	substitute_depth(input, vars, 0, spent)
}

/// Inner substitution carrying the current nesting `depth` so recursive
/// interpolation of modifier defaults/alternates (`${A:-${B}}`) is bounded.
pub(super) fn substitute_depth(
	input: &str,
	vars: &HashMap<String, String>,
	depth: usize,
	spent: &mut usize,
) -> Result<String> {
	if depth > MAX_INTERP_DEPTH {
		return Err(ComposeError::InvalidSubstitution(format!(
			"interpolation nesting too deep (more than {MAX_INTERP_DEPTH} levels)"
		)));
	}

	let mut out = String::with_capacity(input.len());
	let mut chars = input.chars().peekable();

	while let Some(ch) = chars.next() {
		if ch != '$' {
			out.push(ch);
			continue;
		}

		match chars.peek() {
			None => {
				out.push('$');
			}
			Some('$') => {
				chars.next();
				out.push('$');
			}
			Some('{') => {
				chars.next();
				let (var, modifier) = parse_braced_var(&mut chars)?;
				let value = resolve_modifier(var.clone(), modifier, vars, depth, spent)?;
				check_output_cap(&out, &value, &var)?;
				check_document_budget(spent, &value, &var)?;
				out.push_str(&value);
			}
			Some(c) if is_var_start(*c) => {
				let var = collect_var_name(&mut chars);
				let value = match vars.get(&var) {
					Some(v) => v.clone(),
					None => {
						// Match docker compose v2: warn before defaulting to blank.
						tracing::warn!(
							"The {var} variable is not set. Defaulting to a blank string."
						);
						String::new()
					}
				};
				check_output_cap(&out, &value, &var)?;
				check_document_budget(spent, &value, &var)?;
				out.push_str(&value);
			}
			Some(_) => {
				out.push('$');
			}
		}
	}

	Ok(out)
}

/// Refuse a variable expansion that would push the cumulative interpolation
/// output past [`MAX_INTERP_OUTPUT_BYTES`]. Called before the `push_str` so the
/// refusal fires before the offending allocation, with a message naming the
/// variable and the size it had reached.
fn check_document_budget(spent: &mut usize, value: &str, var: &str) -> Result<()> {
	*spent = spent.saturating_add(value.len());
	if *spent > MAX_INTERP_DOCUMENT_BYTES {
		return Err(ComposeError::InvalidSubstitution(format!(
			"interpolating '{var}' pushes this document past {MAX_INTERP_DOCUMENT_BYTES} \
			 bytes of substituted output; at most that much is allowed across the whole \
			 file; the value may repeat across many fields"
		)));
	}
	Ok(())
}

fn check_output_cap(out: &str, value: &str, var: &str) -> Result<()> {
	let new_len = out.len().saturating_add(value.len());
	if new_len > MAX_INTERP_OUTPUT_BYTES {
		return Err(ComposeError::InvalidSubstitution(format!(
			"variable '{var}' would expand to {new_len} bytes of output; at most \
			 {MAX_INTERP_OUTPUT_BYTES} bytes of interpolated output are allowed; the value \
			 may repeat too many times in the document"
		)));
	}
	Ok(())
}

/// Load a `.env` file from `dir`.
///
/// - Lines starting with `#` are comments and are skipped.
/// - Empty / whitespace-only lines are skipped.
/// - `KEY=VALUE` sets KEY to VALUE; surrounding quotes are stripped and
///   dotenv escapes/inline comments are handled.
/// - `KEY` without `=` sets KEY to empty string.
/// - Process environment variables take precedence: if a key already exists in
///   the current process env it will *not* be overridden by the `.env` file.
pub fn load_dotenv(dir: &Path) -> HashMap<String, String> {
	let path = dir.join(".env");
	let Ok(content) = crate::filesystem::read_to_string_capped(&path) else {
		return HashMap::new();
	};

	let mut map = HashMap::new();
	for (key, value) in crate::dotenv::parse(&content) {
		// Process environment variables take precedence over the `.env` file.
		if std::env::var(&key).is_ok() {
			continue;
		}
		map.insert(key, value);
	}

	map
}

/// Build the full variable map: process env + dotenv (process env wins).
pub fn build_vars(dir: &Path) -> HashMap<String, String> {
	let mut vars: HashMap<String, String> = std::env::vars().collect();
	for (k, v) in load_dotenv(dir) {
		vars.entry(k).or_insert(v);
	}
	vars
}

/// Build vars, layering explicit `--env-file` files over the process environment.
///
/// Compose v2 semantics: when one or more `--env-file` are given they *replace*
/// the default `.env` (which is therefore not loaded), and among several files
/// the **last** one wins. With no explicit files this is just [`build_vars`]
/// (process env + `.env`). Process env always takes precedence over file values.
///
/// A missing, unreadable, or malformed `--env-file` is silently skipped here
/// (legacy lenient behaviour). This signature is part of the published library
/// API and is kept for backward compatibility; the CLI drives
/// [`build_vars_with_env_files_strict`], which fails loudly on a bad file.
pub fn build_vars_with_env_files(dir: &Path, extra: &[String]) -> HashMap<String, String> {
	// `strict = false` can never produce an error.
	build_vars_with_env_files_inner(dir, extra, false).unwrap_or_default()
}

/// Like [`build_vars_with_env_files`] but rejects a bad `--env-file`.
///
/// An explicitly-passed `--env-file` that is missing, unreadable, or malformed
/// is a hard error (matching docker compose, which fails on a not-found env
/// file) rather than being silently skipped: a typo'd path must not fall back
/// to process-env/defaults and exit 0.
pub fn build_vars_with_env_files_strict(
	dir: &Path,
	extra: &[String],
) -> Result<HashMap<String, String>> {
	build_vars_with_env_files_inner(dir, extra, true)
}

/// The first control character in `value` that is never legitimate in an
/// env-file value, or `None` if there is none. Tab, newline and carriage return
/// are allowed: dotenv escapes (`\t`, `\n`, `\r`) and multi-line quoted values
/// produce them legitimately, and post-parse interpolation stores them verbatim
/// as scalar data. Everything else in the C0/C1 ranges (NUL, ESC, …) is
/// rejected. Pure so it is unit-tested.
fn first_disallowed_control_char(value: &str) -> Option<char> {
	value
		.chars()
		.find(|&c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
}

fn build_vars_with_env_files_inner(
	dir: &Path,
	extra: &[String],
	strict: bool,
) -> Result<HashMap<String, String>> {
	if extra.is_empty() {
		return Ok(build_vars(dir));
	}

	// Explicit `--env-file`s replace `.env`; a later file overrides an earlier one.
	let mut file_vars: HashMap<String, String> = HashMap::new();
	for path in extra {
		let abs = if std::path::Path::new(path).is_absolute() {
			std::path::PathBuf::from(path)
		} else {
			dir.join(path)
		};
		let content = match crate::filesystem::read_to_string_capped(&abs) {
			Ok(content) => content,
			Err(e) => {
				if strict {
					return Err(crate::error::ComposeError::EnvFile(format!(
						"env file not found: {} ({e})",
						abs.display()
					)));
				}
				continue;
			}
		};
		let pairs = if strict {
			crate::dotenv::parse_strict(&content)?
		} else {
			crate::dotenv::parse(&content)
		};
		for (key, value) in pairs {
			// A disallowed control character (e.g. NUL) in a value would be
			// interpolated verbatim into a compose scalar, where it is meaningless
			// at best and corrupts the container's config at worst. Reject it here,
			// at load time, with an error that names the originating env file and
			// key, instead of letting it surface later as a compose-file parse
			// error at a meaningless post-substitution offset. Only the explicit
			// (strict) `--env-file`/`env_file:` path errors; the lenient `.env`
			// fallback keeps its historical pass-through behaviour.
			if strict {
				if let Some(bad) = first_disallowed_control_char(&value) {
					return Err(crate::error::ComposeError::EnvFile(format!(
						"env file {}: value of '{key}' contains a disallowed control \
						 character ({}); remove it before use",
						abs.display(),
						bad.escape_default(),
					)));
				}
			}
			file_vars.insert(key, value);
		}
	}

	// Process env wins over every file value.
	let mut vars: HashMap<String, String> = std::env::vars().collect();
	for (k, v) in file_vars {
		vars.entry(k).or_insert(v);
	}
	Ok(vars)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
