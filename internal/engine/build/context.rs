//! Build context tar assembly and ignore-file matching (`.containerignore` / `.dockerignore`).
//!
//! Walks the build context directory, applies ignore semantics (last-match-wins
//! with `!` re-includes, `*`/`?`/`**` globs), and packs the result into a
//! gzipped tar suitable for the libpod build endpoint.
//!
//! Podman reads `.containerignore` or `.dockerignore` and prefers its own when
//! both exist; exactly one applies, never the union. See [`ignore_file`].

use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;

use crate::engine::walk;
use crate::error::{ComposeError, Result};

/// Append the build context to `tar`, honoring `.dockerignore`.
///
/// `skip_names` are context-relative paths to omit even when not ignored (used to
/// drop the user's `.dockerignore` so it can be rewritten with extra rules).
fn append_context<W: std::io::Write>(
	tar: &mut tar::Builder<W>,
	context: &Path,
	ignore_patterns: &[String],
	skip_names: &[&str],
	force_include: &[&str],
) -> Result<()> {
	// Do not dereference symlinks: a symlink in the context would otherwise pack
	// the bytes of its (possibly out-of-context) target — e.g. `/etc/hostname` or
	// an SSH key — into the image. Store the link itself instead, matching the
	// watch-sync and cp paths.
	tar.follow_symlinks(false);
	for abs in walk::walk_dir(context).map_err(ComposeError::Io)? {
		let rel = abs
			.strip_prefix(context)
			.map_err(|_| ComposeError::Build("path strip error".into()))?;
		let rel_str = rel.to_string_lossy();
		if skip_names.iter().any(|n| rel_str == *n) {
			continue;
		}
		// The active Dockerfile is always sent to the builder even when
		// `.dockerignore` would match it: Docker keeps the Dockerfile (and
		// `.dockerignore`) available for the build itself — they just can't be
		// COPY'd into the image. Without this, a `.dockerignore` listing
		// `Dockerfile` (or a blanket `*`) drops it from the context tar and the
		// build fails with "stat .../Dockerfile: no such file or directory".
		let forced = force_include.iter().any(|n| rel_str == *n);
		if !forced && is_ignored(&rel_str, ignore_patterns) {
			continue;
		}
		// Classify without following symlinks so a symlink-to-dir is stored as a
		// link rather than walked and dereferenced.
		let is_dir = abs.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false);
		if is_dir {
			tar.append_dir(rel, &abs)
				.map_err(|e| ComposeError::Build(e.to_string()))?;
		} else {
			tar.append_path_with_name(&abs, rel)
				.map_err(|e| ComposeError::Build(e.to_string()))?;
		}
	}
	Ok(())
}

/// Append synthesized files (e.g. build secrets) to `tar` at the context root.
///
/// Build-secret bytes are placed in the build context as `.podup-build-secret-*`
/// entries so the libpod build endpoint can expose them through its BuildKit
/// `secrets=id=NAME,src=ENTRY` mount; they ride inside the context tar by design
/// of that mount mechanism and are not part of the user's source tree.
fn append_extra_files<W: std::io::Write>(
	tar: &mut tar::Builder<W>,
	extra_files: &[(String, Vec<u8>)],
) -> Result<()> {
	for (name, bytes) in extra_files {
		let mut header = tar::Header::new_gnu();
		header.set_size(bytes.len() as u64);
		header.set_mode(0o600);
		header.set_cksum();
		tar.append_data(&mut header, name, bytes.as_slice())
			.map_err(|e| ComposeError::Build(e.to_string()))?;
	}
	Ok(())
}

/// The `.dockerfile-inline` entry name synthesized for an inline Dockerfile.
pub(super) const INLINE_DOCKERFILE_NAME: &str = ".dockerfile-inline";

/// Assemble the gzipped build-context tar and write it straight to `writer`.
///
/// This is the shared core of context assembly. `build_context_tar` wraps it
/// to collect the bytes into a `Vec` (tests, non-streaming callers); the build
/// path feeds a channel-backed writer so a multi-gigabyte context is streamed to
/// the socket without ever inflating the process's RSS.
pub(super) fn stream_build_context<W: std::io::Write>(
	writer: W,
	context: &Path,
	dockerfile: &str,
	extra_files: &[(String, Vec<u8>)],
) -> Result<()> {
	let (ignore_name, ignore_patterns) = ignore_file(context);
	let encoder = GzEncoder::new(writer, Compression::default());
	let mut tar = tar::Builder::new(encoder);

	// Force-include the active Dockerfile so an ignore file that matches it
	// cannot drop it from the context the builder receives (Docker parity).
	if extra_files.is_empty() {
		append_context(&mut tar, context, &ignore_patterns, &[], &[dockerfile])?;
	} else {
		// Skip the user's ignore file; it is rewritten with an exclusion per
		// synthesized entry so a `COPY .` in the build cannot bake secret bytes
		// into image layers. It must be skipped and re-emitted under the same
		// name the server will read, or the exclusions never apply.
		append_context(
			&mut tar,
			context,
			&ignore_patterns,
			&[ignore_name],
			&[dockerfile],
		)?;
		let synthesized: Vec<&str> = extra_files.iter().map(|(n, _)| n.as_str()).collect();
		append_ignore_file(
			&mut tar,
			ignore_name,
			&synthesized_ignore_file(context, ignore_name, &synthesized),
		)?;
	}
	append_extra_files(&mut tar, extra_files)?;
	finish_tar(tar)
}

/// As [`stream_build_context`], but injects an inline Dockerfile as
/// [`INLINE_DOCKERFILE_NAME`] instead of shipping one from the context.
pub(super) fn stream_build_context_with_inline<W: std::io::Write>(
	writer: W,
	context: &Path,
	inline: &str,
	extra_files: &[(String, Vec<u8>)],
) -> Result<()> {
	let (ignore_name, ignore_patterns) = ignore_file(context);
	let encoder = GzEncoder::new(writer, Compression::default());
	let mut tar = tar::Builder::new(encoder);

	let mut header = tar::Header::new_gnu();
	header.set_size(inline.len() as u64);
	header.set_mode(0o644);
	header.set_cksum();
	tar.append_data(&mut header, INLINE_DOCKERFILE_NAME, inline.as_bytes())
		.map_err(|e| ComposeError::Build(e.to_string()))?;

	// Skip the user's ignore file here; it is rewritten below with extra rules
	// excluding every synthesized entry (inline Dockerfile, build secrets), under
	// the same name the server will read.
	append_context(&mut tar, context, &ignore_patterns, &[ignore_name], &[])?;

	let mut synthesized: Vec<&str> = vec![INLINE_DOCKERFILE_NAME];
	synthesized.extend(extra_files.iter().map(|(n, _)| n.as_str()));
	append_ignore_file(
		&mut tar,
		ignore_name,
		&synthesized_ignore_file(context, ignore_name, &synthesized),
	)?;

	append_extra_files(&mut tar, extra_files)?;
	finish_tar(tar)
}

/// Finish the gzip stream and flush the sink. `GzEncoder::finish` writes the
/// trailer but does not flush the underlying writer, so a channel-backed writer
/// would strand its last buffered chunk without this explicit flush.
fn finish_tar<W: std::io::Write>(tar: tar::Builder<GzEncoder<W>>) -> Result<()> {
	let gz = tar
		.into_inner()
		.map_err(|e| ComposeError::Build(e.to_string()))?;
	let mut writer = gz
		.finish()
		.map_err(|e| ComposeError::Build(e.to_string()))?;
	writer
		.flush()
		.map_err(|e| ComposeError::Build(e.to_string()))?;
	Ok(())
}

/// Collect [`stream_build_context_with_inline`] into a `Vec`. Test-only: the
/// build path streams the tar; these wrappers let the tar assembly be asserted
/// on its bytes.
#[cfg(test)]
pub(super) fn build_context_tar_with_inline(
	context: &Path,
	inline: &str,
	extra_files: &[(String, Vec<u8>)],
) -> Result<(Vec<u8>, String)> {
	let mut buf = Vec::new();
	stream_build_context_with_inline(&mut buf, context, inline, extra_files)?;
	Ok((buf, INLINE_DOCKERFILE_NAME.to_string()))
}

/// Collect [`stream_build_context`] into a `Vec`. Test-only (see
/// [`build_context_tar_with_inline`]).
#[cfg(test)]
pub(crate) fn build_context_tar(
	context: &Path,
	dockerfile: &str,
	extra_files: &[(String, Vec<u8>)],
) -> Result<Vec<u8>> {
	let mut buf = Vec::new();
	stream_build_context(&mut buf, context, dockerfile, extra_files)?;
	Ok(buf)
}

/// Append a synthesized `.dockerignore` entry to the context tar.
fn append_ignore_file<W: std::io::Write>(
	tar: &mut tar::Builder<W>,
	name: &str,
	content: &str,
) -> Result<()> {
	let mut header = tar::Header::new_gnu();
	header.set_size(content.len() as u64);
	header.set_mode(0o644);
	header.set_cksum();
	tar.append_data(&mut header, name, content.as_bytes())
		.map_err(|e| ComposeError::Build(e.to_string()))
}

/// Build the ignore-file content for a context tar carrying synthesized entries:
/// any user rules plus a final exclusion per synthesized name, so a `COPY .` in
/// the build does not capture the inline Dockerfile or a build secret. The
/// libpod `secrets=id=…,src=…` mount reads straight from the extracted context,
/// which the ignore file does not filter, so excluded secret entries remain
/// mountable.
///
/// `name` is the ignore file the server will read (see [`ignore_file`]); the
/// user rules are carried over from that same file, never from the other one.
fn synthesized_ignore_file(context: &Path, name: &str, names: &[&str]) -> String {
	let existing = crate::filesystem::read_to_string_capped(context.join(name)).unwrap_or_default();
	let mut out = existing.trim_end_matches(['\n', '\r']).to_string();
	for name in names {
		if !out.is_empty() {
			out.push('\n');
		}
		out.push_str(name);
	}
	out.push('\n');
	out
}

/// Map a compose `additional_contexts` value to the libpod
/// `additionalbuildcontexts` form: `image:`, `url:`, or `localpath:`.
pub(super) fn map_additional_context(base_dir: &Path, value: &str) -> String {
	if let Some(img) = value.strip_prefix("docker-image://") {
		format!("image:{img}")
	} else if value.starts_with("http://")
		|| value.starts_with("https://")
		|| value.starts_with("git://")
	{
		format!("url:{value}")
	} else {
		format!("localpath:{}", base_dir.join(value).display())
	}
}

/// The ignore file podman would read for `context`, and its patterns.
///
/// podman-build(1): "If the file `.containerignore` or `.dockerignore` exists in
/// the context directory, podman build reads its contents. […] if both are in
/// the context directory, podman build only uses `.containerignore`."
///
/// So exactly one file applies — never the union. Applying both client-side
/// produced an image missing content that `podman build` puts in, because we
/// filtered by `.dockerignore` (a file podman would have ignored entirely) and
/// the server then filtered by `.containerignore`.
///
/// The returned name is also the name any synthesized ignore file must be
/// written under, or the server would read the *other* one and our exclusions
/// would not apply. When neither file exists, `.containerignore` is the name to
/// synthesize: podman prefers it, so it cannot be shadowed.
fn ignore_file(context: &Path) -> (&'static str, Vec<String>) {
	for name in [".containerignore", ".dockerignore"] {
		let Ok(content) = crate::filesystem::read_to_string_capped(context.join(name)) else {
			continue;
		};
		let patterns = content
			.lines()
			.map(|l| l.trim().to_string())
			.filter(|l| !l.is_empty() && !l.starts_with('#'))
			.collect();
		return (name, patterns);
	}
	(".containerignore", Vec::new())
}

/// Decide whether `path` is excluded from the build context.
///
/// Patterns are evaluated in order and the **last** match wins, matching Docker
/// `.dockerignore` semantics: a leading `!` re-includes a path that an earlier
/// pattern excluded. So `*.log` then `!keep.log` ignores every log except
/// `keep.log`.
fn is_ignored(path: &str, patterns: &[String]) -> bool {
	let mut ignored = false;
	for pattern in patterns {
		let (negated, pat) = match pattern.strip_prefix('!') {
			Some(rest) => (true, rest),
			None => (false, pattern.as_str()),
		};
		if pattern_matches(pat, path) {
			ignored = !negated;
		}
	}
	ignored
}

/// Match a single (already de-negated) `.dockerignore` pattern against `path`.
fn pattern_matches(pattern: &str, path: &str) -> bool {
	if pattern.is_empty() {
		return false;
	}
	// Directory pattern (`foo/`): match the directory and everything beneath it.
	if let Some(dir) = pattern.strip_suffix('/') {
		return path == dir || path.starts_with(&format!("{dir}/"));
	}
	if pattern.contains('*') || pattern.contains('?') {
		return glob_match(pattern, path);
	}
	// Plain pattern: exact match, or a path segment prefix (`vendor` matches
	// `vendor/lib.rs`).
	path == pattern
		|| (path.starts_with(pattern) && path.as_bytes().get(pattern.len()) == Some(&b'/'))
}

/// Match path against a glob pattern.
///
/// Patterns without `/` are matched against the filename only, so `*.log`
/// excludes both `error.log` and `logs/error.log`. A single `*` never crosses a
/// `/` boundary; `**` matches any number of path segments (including `/`), so
/// `**/*.key` and `a/**/b` work like Docker.
fn glob_match(pattern: &str, path: &str) -> bool {
	if !pattern.contains('/') && !pattern.contains("**") {
		let filename = path.rsplit('/').next().unwrap_or(path);
		return glob_rec(pattern.as_bytes(), filename.as_bytes());
	}
	glob_rec(pattern.as_bytes(), path.as_bytes())
}

/// Backtracking glob matcher: `?` matches one non-`/` char, `*` matches any run
/// of non-`/` chars, `**` matches any run including `/`.
fn glob_rec(pat: &[u8], s: &[u8]) -> bool {
	if pat.is_empty() {
		return s.is_empty();
	}
	// `**` — matches across `/` boundaries.
	if pat.starts_with(b"**") {
		let mut rest = &pat[2..];
		// `**/` may also match zero directories, so `**/foo` matches top-level `foo`.
		if rest.first() == Some(&b'/') && glob_rec(&rest[1..], s) {
			return true;
		}
		if rest.is_empty() {
			rest = b"";
		}
		// Try consuming any prefix of `s` (including `/`).
		for i in 0..=s.len() {
			if glob_rec(rest, &s[i..]) {
				return true;
			}
		}
		return false;
	}
	match pat[0] {
		b'*' => {
			// Match any run of non-`/` chars.
			let mut i = 0;
			loop {
				if glob_rec(&pat[1..], &s[i..]) {
					return true;
				}
				if i >= s.len() || s[i] == b'/' {
					return false;
				}
				i += 1;
			}
		}
		b'?' => !s.is_empty() && s[0] != b'/' && glob_rec(&pat[1..], &s[1..]),
		c => !s.is_empty() && s[0] == c && glob_rec(&pat[1..], &s[1..]),
	}
}

#[cfg(test)]
mod tests;
