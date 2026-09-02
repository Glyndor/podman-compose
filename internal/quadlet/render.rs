//! Render compose values into Quadlet directive strings, and the `[Section]`
//! accumulator plus the sanitizing helpers shared across the generator.

use std::collections::BTreeMap;

use crate::compose::types::{Command, RestartPolicy, VolumeMount, VolumeType};
use crate::ports::ParsedPort;

/// Render a parsed port into a Quadlet `PublishPort=` value
/// (`[host_ip:][host_port:]container[/proto]`). A missing or `0` host port is
/// omitted so Podman picks one; the protocol suffix is only added when it is not
/// the default `tcp`.
pub(super) fn render_publish_port(p: &ParsedPort) -> String {
	let mut s = String::new();
	if !p.host_ip.is_empty() {
		s.push_str(&p.host_ip);
		s.push(':');
	}
	// A host port of None/0 means "let Podman pick"; omit it so the published
	// side is empty (PublishPort=<container>) and Podman assigns a port.
	if let Some(host) = p.host_port.filter(|n| *n != 0) {
		s.push_str(&host.to_string());
		s.push(':');
	}
	s.push_str(&p.container_port.to_string());
	if p.protocol != "tcp" {
		s.push('/');
		s.push_str(&p.protocol);
	}
	s
}

/// Render a volume mount into a Quadlet `Volume=` value. A source naming a
/// declared volume gets a project-prefixed `.volume` suffix (so Quadlet wires it
/// to the generated unit, whose file name is also project-prefixed); an
/// undeclared source passes through verbatim. An empty source renders as just the
/// target. Long-form `read_only`, SELinux relabel (`z`/`Z`), bind propagation,
/// and `nocopy` opts are folded into the trailing `:opt,opt` field.
pub(super) fn render_volume(vol: &VolumeMount, project: &str, declared_volumes: &[&str]) -> String {
	match vol {
		VolumeMount::Short(s) => {
			let parts: Vec<&str> = s.splitn(3, ':').collect();
			if parts.len() >= 2 && declared_volumes.contains(&parts[0]) {
				let mut out = format!("{}.volume:{}", unit_stem(project, parts[0]), parts[1]);
				if let Some(opts) = parts.get(2) {
					out.push(':');
					out.push_str(opts);
				}
				out
			} else {
				s.clone()
			}
		}
		VolumeMount::Long {
			source,
			target,
			read_only,
			bind,
			volume,
			..
		} => {
			let src = source.clone().unwrap_or_default();
			let src = if declared_volumes.contains(&src.as_str()) {
				format!("{}.volume", unit_stem(project, &src))
			} else {
				src
			};
			// Collect mount options into the trailing `:opt,opt` field so nothing
			// is dropped: SELinux relabel (`z`/`Z`) and bind propagation are
			// security- and correctness-relevant on hardened hosts.
			let mut opts: Vec<String> = Vec::new();
			if *read_only == Some(true) {
				opts.push("ro".to_string());
			}
			if let Some(b) = bind {
				if let Some(selinux) = &b.selinux {
					opts.push(selinux.clone());
				}
				if let Some(propagation) = &b.propagation {
					opts.push(propagation.clone());
				}
			}
			if let Some(v) = volume {
				if v.nocopy == Some(true) {
					opts.push("nocopy".to_string());
				}
				// The mount-hardening trio (#1160). Dropping these here would
				// mean a compose file that hardens a mount exports a Quadlet
				// unit that silently does not.
				if v.noexec == Some(true) {
					opts.push("noexec".to_string());
				}
				if v.nosuid == Some(true) {
					opts.push("nosuid".to_string());
				}
				if v.nodev == Some(true) {
					opts.push("nodev".to_string());
				}
			}
			let mut out = if src.is_empty() {
				target.clone()
			} else {
				format!("{src}:{target}")
			};
			if !opts.is_empty() {
				out.push(':');
				out.push_str(&opts.join(","));
			}
			out
		}
	}
}

/// If `vol` is a long-form `type: tmpfs` mount, render it as a Quadlet `Tmpfs=`
/// value (`target[:size=…,mode=…]`); otherwise return `None` so the caller emits
/// a normal `Volume=`. A tmpfs mount written as `Volume=` would create a
/// persistent named/anonymous volume instead of an in-memory filesystem — a
/// silent semantic inversion. Size/mode mirror the runtime mount options.
pub(super) fn render_tmpfs_mount(vol: &VolumeMount) -> Option<String> {
	let VolumeMount::Long {
		volume_type: VolumeType::Tmpfs,
		target,
		tmpfs,
		..
	} = vol
	else {
		return None;
	};
	let mut opts: Vec<String> = Vec::new();
	if let Some(t) = tmpfs {
		if let Some(size) = t.size {
			opts.push(format!("size={size}"));
		}
		if let Some(mode) = t.mode {
			opts.push(format!("mode={mode:o}"));
		}
	}
	if opts.is_empty() {
		Some(target.clone())
	} else {
		Some(format!("{target}:{}", opts.join(",")))
	}
}

/// Render a compose `command:`/`entrypoint:` into a single-line systemd `Exec=`
/// value. systemd splits the value on whitespace honouring double-quoted groups
/// and C-style escapes, so each list element is quoted when it contains
/// whitespace or quoting characters, and control characters are always escaped.
/// Without this a multi-line argument (e.g. an `sh -c` block scalar) would emit
/// raw newlines that spill onto extra physical lines and corrupt the unit file.
pub(super) fn render_command(command: &Command) -> String {
	match command {
		// A shell-form command is already a single command line; keep its word
		// splitting but neutralise control characters so it stays on one line.
		Command::Shell(s) => escape_exec_control(s),
		Command::Exec(parts) => parts
			.iter()
			.map(|p| quote_exec_arg(p))
			.collect::<Vec<_>>()
			.join(" "),
	}
}

/// C-escape the characters that must never appear raw in a systemd command line.
fn escape_exec_control(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for c in s.chars() {
		match c {
			'\\' => out.push_str("\\\\"),
			'\n' => out.push_str("\\n"),
			'\r' => out.push_str("\\r"),
			'\t' => out.push_str("\\t"),
			_ => out.push(c),
		}
	}
	out
}

/// Quote a single `Exec=` argument: wrap it in double quotes (escaping `"` and
/// control characters) when it is empty or contains whitespace or quoting
/// characters, so systemd keeps it as one argument. Plain arguments pass through
/// unchanged.
fn quote_exec_arg(arg: &str) -> String {
	let needs_quoting = arg.is_empty()
		|| arg
			.chars()
			.any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | '$' | '%' | ';'));
	if needs_quoting {
		format!("\"{}\"", escape_exec_control(arg).replace('"', "\\\""))
	} else {
		arg.to_string()
	}
}

/// Map a compose `restart:` policy onto a systemd `Restart=` value.
/// `unless-stopped` has no systemd equivalent and is treated as `always`.
pub(super) fn render_restart(restart: &RestartPolicy) -> String {
	match restart {
		RestartPolicy::No => "no".to_string(),
		RestartPolicy::Always => "always".to_string(),
		RestartPolicy::UnlessStopped => "always".to_string(),
		RestartPolicy::OnFailure { .. } => "on-failure".to_string(),
	}
}

/// Sort a map of optional-valued entries by key into a stable `Vec`, so the
/// generated unit is byte-deterministic regardless of `HashMap` iteration order.
pub(super) fn sorted_pairs(
	map: std::collections::HashMap<String, Option<String>>,
) -> Vec<(String, Option<String>)> {
	let sorted: BTreeMap<_, _> = map.into_iter().collect();
	sorted.into_iter().collect()
}

/// Sort a map of string-valued entries (labels, sysctls, …) by key into a stable
/// `Vec`, so the generated unit is byte-deterministic regardless of `HashMap`
/// iteration order.
pub(super) fn sorted_label_pairs(
	map: std::collections::HashMap<String, String>,
) -> Vec<(String, String)> {
	let sorted: BTreeMap<_, _> = map.into_iter().collect();
	sorted.into_iter().collect()
}

/// Strip ASCII control characters from a value before it is written into a
/// unit file. systemd unit entries are single-line `Key=Value` pairs; an
/// embedded newline from a hostile compose field would otherwise inject
/// arbitrary unit directives (e.g. a `[Service]` `ExecStartPre=`). Compose
/// input is untrusted, so every dynamic value is sanitized at the boundary.
pub(super) fn sanitize_value(value: &str) -> String {
	value.chars().filter(|c| !c.is_control()).collect()
}

/// Keys whose value Quadlet/systemd splits on whitespace (a `KEY=VALUE` option
/// list): an unquoted space would split a single value into a truncated value
/// plus bogus extra entries, so such values are quoted when they contain
/// whitespace.
fn key_is_word_split(key: &str) -> bool {
	matches!(key, "Environment" | "Label" | "Annotation" | "Sysctl")
}

/// Keys whose value is an argument line we render (and quote) ourselves
/// (`PodmanArgs`, `Exec`, `Entrypoint`); systemd must keep splitting these on
/// whitespace, so they are passed through with only control characters stripped.
fn key_is_arg_line(key: &str) -> bool {
	matches!(key, "PodmanArgs" | "Exec" | "Entrypoint")
}

/// Escape a value for a single-line systemd `Key=Value` entry.
///
/// On top of [`sanitize_value`] (control characters stripped so a value can
/// never inject a second directive) this makes the value byte-faithful to the
/// compose source the way docker-compose treats it:
///
/// * systemd specifiers (`%h`, `%U`, …) are passed through literally by doubling
///   `%` to `%%`, instead of being expanded at unit-activation time;
/// * a value ending in a backslash — which would otherwise fold the next
///   physical line (and the directive on it) into this value — is quoted and the
///   backslash escaped, closing the line-continuation hole;
/// * for word-split keys (`Environment`, `Label`, …) a value containing
///   whitespace is double-quoted so systemd keeps it as one value rather than
///   splitting it into bogus extra entries.
///
/// Argument-line keys keep their own rendering (they encode whitespace splitting
/// deliberately) and only have control characters stripped.
pub(super) fn escape_unit_value(key: &str, value: &str) -> String {
	let stripped = sanitize_value(value);
	if key_is_arg_line(key) {
		return stripped;
	}
	let escaped = stripped.replace('%', "%%");
	let needs_quote = (key_is_word_split(key) && escaped.contains(char::is_whitespace))
		|| escaped.ends_with('\\')
		|| escaped.starts_with('"');
	if needs_quote {
		format!("\"{}\"", escaped.replace('\\', "\\\\").replace('"', "\\\""))
	} else {
		escaped
	}
}

/// Build the project-prefixed unit-file stem for a resource, e.g. service `web`
/// in project `proj` → `proj-web`. Generated unit files share a single systemd
/// directory across projects, so the stem (and every in-unit cross-reference to
/// it) carries the project name — matching the project-scoped resource names
/// inside the units — so two projects' `web` services do not clobber each other.
pub(super) fn unit_stem(project: &str, name: &str) -> String {
	safe_unit_stem(&format!("{project}-{name}"))
}

/// Reduce a compose key to a safe single path-component stem for a unit file
/// name. Keeps ASCII alphanumerics and `-`/`_`/`.`; replaces anything else
/// (path separators, control characters) with `_`, and guarantees the result
/// is non-empty and starts with neither a dot nor a dash, so it can never escape
/// the output directory, resolve to `.`/`..`, or be mistaken for a command-line
/// flag by downstream tooling that globs the generated file names.
pub(super) fn safe_unit_stem(name: &str) -> String {
	let mut stem: String = name
		.chars()
		.map(|c| {
			if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
				c
			} else {
				'_'
			}
		})
		.collect();
	if stem.is_empty() || stem.starts_with('.') || stem.starts_with('-') {
		stem.insert(0, '_');
	}
	stem
}

/// A single `[Section]` accumulating `Key=Value` lines in insertion order.
pub(super) struct Section {
	name: &'static str,
	lines: Vec<String>,
}

impl Section {
	/// Start an empty section with the given `[name]` header.
	pub(super) fn new(name: &'static str) -> Self {
		Section {
			name,
			lines: Vec::new(),
		}
	}

	/// Append a `Key=Value` line, escaping the value for systemd unit syntax
	/// (control characters stripped, specifiers passed through literally,
	/// whitespace/line-continuation hazards quoted) so a hostile or merely
	/// awkward compose field cannot inject extra directives, split a value, or
	/// swallow the following line. See [`escape_unit_value`].
	pub(super) fn add(&mut self, key: &str, value: String) {
		self.lines
			.push(format!("{key}={}", escape_unit_value(key, &value)));
	}

	/// True when no lines have been added; lets the caller skip emitting an empty
	/// section.
	pub(super) fn is_empty(&self) -> bool {
		self.lines.is_empty()
	}

	/// Render the section as `[name]` followed by its lines, each newline-terminated.
	pub(super) fn render(&self) -> String {
		let mut s = format!("[{}]\n", self.name);
		for line in &self.lines {
			s.push_str(line);
			s.push('\n');
		}
		s
	}
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
