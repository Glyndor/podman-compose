//! Argv-tokenisation helpers used by tests to assert that a generated unit's
//! `PodmanArgs=` value cannot smuggle an extra `podman run` flag into the argv
//! quadlet's generator would build from it.
//!
//! The unit text alone is not enough: `PodmanArgs=--memory=512m --privileged`
//! is what `escape_unit_value` produced on the unfixed tree for a hostile
//! `mem_limit: "512m --privileged"`, and it reads as one innocent-looking
//! line. Podman, however, would receive `--memory=512m` and `--privileged` as
//! two separate argv elements. That is the security property the bug breaks,
//! and the only one worth asserting.
//!
//! systemd's `systemd.syntax(7)` word-splitter is the authoritative divider:
//! the directive value after `PodmanArgs=` is tokenised on whitespace while
//! honouring `"`-quoted groups (single quotes and C-style escapes are also
//! honoured by systemd, but the seven interpolation sites have no reason to
//! emit those, so a faithful `PodmanArgs=-side only needs the double-quote
//! form to be tested).

/// Tokenise one `PodmanArgs=` value the way systemd does.
///
/// Returns the argv podman would receive. Tokens coming out of a `"..."`
/// group have their quotes stripped, the way systemd does (the consumed
/// double quotes are not preserved as characters).
pub fn tokenise_podman_argv(value: &str) -> Vec<String> {
	let mut out = Vec::new();
	let mut current = String::new();
	let mut in_quotes = false;
	let mut chars = value.chars().peekable();
	while let Some(c) = chars.next() {
		if in_quotes {
			match c {
				'"' => in_quotes = false,
				'\\' if chars.peek() == Some(&'"') => {
					current.push('"');
					chars.next();
				}
				_ => current.push(c),
			}
			continue;
		}
		match c {
			'"' => in_quotes = true,
			c if c.is_whitespace() => {
				if !current.is_empty() {
					out.push(std::mem::take(&mut current));
				}
			}
			_ => current.push(c),
		}
	}
	if !current.is_empty() {
		out.push(current);
	}
	out
}

/// Parse every `PodmanArgs=` line out of a unit body, concatenate the values
/// the way systemd does (`PodmanArgs=` is multi-valued by design), and
/// tokenise the combined string. Returns the argv podman would receive when
/// the generator feeds it to `podman run`.
///
/// Comment lines (`#`) are skipped so a unit file's ownership marker does not
/// confuse the parser; the `# podup-owner: <project>` line lives at the top
/// of every generated unit and would otherwise contribute empty tokens.
pub fn podman_argv_from_unit(unit_contents: &str) -> Vec<String> {
	let mut combined = String::new();
	for raw in unit_contents.lines() {
		let line = raw.trim_end();
		if line.starts_with('#') {
			continue;
		}
		if let Some(rest) = line.strip_prefix("PodmanArgs=") {
			if !combined.is_empty() {
				combined.push('\n');
			}
			combined.push_str(rest);
		}
	}
	tokenise_podman_argv(&combined)
}

/// Assert that no `PodmanArgs=` line in `unit_contents` produces `needle` as
/// its own argv element after systemd tokenisation. Used by the seven
/// per-site tests to pin the property the bug breaks.
pub fn assert_argv_has_no_token(unit_contents: &str, needle: &str) {
	let argv = podman_argv_from_unit(unit_contents);
	assert!(
		!argv.iter().any(|tok| tok == needle),
		"unit produces {needle:?} as its own argv element; argv = {argv:#?}; unit:\n{unit_contents}"
	);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn tokenise_splits_on_whitespace() {
		assert_eq!(
			tokenise_podman_argv("--memory=512m --privileged"),
			vec!["--memory=512m", "--privileged"]
		);
	}

	#[test]
	fn tokenise_respects_double_quotes() {
		// `--cpuset-cpus="0 --privileged"` is ONE argv element. That is what
		// the seven-site fix must produce, and what this helper proves.
		assert_eq!(
			tokenise_podman_argv(r#"--cpuset-cpus="0 --privileged""#),
			vec!["--cpuset-cpus=0 --privileged"]
		);
	}

	#[test]
	fn multiple_podman_args_lines_concatenate() {
		// systemd accepts multiple `PodmanArgs=` lines; their values merge.
		let unit = "\
PodmanArgs=--memory=512m
PodmanArgs=--cpuset-cpus=0,1
";
		assert_eq!(
			podman_argv_from_unit(unit),
			vec!["--memory=512m", "--cpuset-cpus=0,1"]
		);
	}

	#[test]
	fn comments_are_ignored() {
		// The podup ownership marker is the first line of every unit.
		let unit = "# podup-owner: p\nPodmanArgs=--privileged\n";
		assert_eq!(podman_argv_from_unit(unit), vec!["--privileged"]);
	}
}
