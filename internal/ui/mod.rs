//! Terminal styling — the single place that decides whether to colour output and
//! holds the palette, so colour stays consistent and always honours `--ansi`,
//! `NO_COLOR`, and whether the target stream is a TTY.
//!
//! Two kinds of sink exist in podup:
//! - Sinks written through [`anstream`] (e.g. [`anstream::stdout`]) strip the
//!   ANSI codes themselves when colour is off, so callers may always emit codes.
//! - Raw sinks (the tracing writer, the log-prefixer's borrowed stdout) do not
//!   strip, so those callers gate on `stdout_colored`/`stderr_colored`.
//!
//! `set_color_choice` also constructs the auto streams once so anstream enables
//! virtual-terminal processing on Windows for the rest of the process.

use std::io::IsTerminal;

pub use anstyle::{AnsiColor, Style};

pub use anstream::ColorChoice;

mod table;
pub use table::{fit_cell, sanitize_cell, Table};

/// Apply the resolved colour choice process-wide and enable Windows VT once.
///
/// anstream's `stdout()`/`stderr()` then honour this choice together with
/// `NO_COLOR`/`CLICOLOR` and TTY detection.
pub fn set_color_choice(choice: ColorChoice) {
	choice.write_global();
	// Constructing the auto streams enables virtual-terminal processing on
	// Windows so raw-sink callers can emit ANSI safely; a no-op elsewhere.
	let _ = anstream::AutoStream::auto(std::io::stdout());
	let _ = anstream::AutoStream::auto(std::io::stderr());
}

/// `NO_COLOR` is set to a non-empty value (the no-color.org convention).
fn no_color() -> bool {
	std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Whether a stream that is (or isn't) a TTY should be coloured under `choice`.
/// `Auto` defers to the TTY and `NO_COLOR`. Pure (takes the choice as an argument)
/// so the policy is tested without mutating the process-global choice.
fn colored_with(choice: ColorChoice, is_terminal: bool) -> bool {
	match choice {
		ColorChoice::Always | ColorChoice::AlwaysAnsi => true,
		ColorChoice::Never => false,
		ColorChoice::Auto => is_terminal && !no_color(),
	}
}

/// Whether a stream that is (or isn't) a TTY should be coloured under the global
/// choice.
fn colored(is_terminal: bool) -> bool {
	colored_with(ColorChoice::global(), is_terminal)
}

/// Whether a raw (non-anstream) write to stdout should embed ANSI codes.
pub fn stdout_colored() -> bool {
	colored(std::io::stdout().is_terminal())
}

/// Whether a raw (non-anstream) write to stderr should embed ANSI codes.
pub fn stderr_colored() -> bool {
	colored(std::io::stderr().is_terminal())
}

/// Process-wide switch for user-facing lifecycle progress output. Off by default
/// so the library (consumed by other crates) stays silent unless asked; the CLI
/// turns it on for the human-facing lifecycle commands. Progress is written to
/// stderr (matching docker compose) so stdout stays a clean pipe for scripting
/// and machine/JSON output.
static PROGRESS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The project name, so an identity colour can be keyed on the same label
/// everywhere.
///
/// `logs` prefixes lines with the project-stripped `web-1`; `ps` prints the full
/// container name `proj-web-1`; the progress lines print the full name too.
/// Hashing whatever string each site happens to hold gives the same container a
/// different colour in each command — which defeats the point of a stable
/// palette. Stripping the project first makes one container one colour.
static PROJECT: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Record the project name for identity colouring. Set once per invocation.
pub fn set_project(name: &str) {
	if let Ok(mut slot) = PROJECT.write() {
		name.clone_into(&mut slot);
	}
}

/// The stable identity colour for a container or service label, keyed on the
/// label with the project prefix removed.
///
/// Callers pass whatever they display — `proj-web-1`, `web-1`, `web` — and get
/// the same colour for the same container, which is what makes `ps`, `logs`,
/// `stats` and the progress lines agree.
pub fn identity_style(label: &str) -> Style {
	let key = PROJECT
		.read()
		.ok()
		.and_then(|p| {
			(!p.is_empty())
				.then(|| label.strip_prefix(&format!("{p}-")).map(str::to_string))
				.flatten()
		})
		.unwrap_or_else(|| label.to_string());
	service_style(strip_replica_suffix(&key))
}

/// Drop a trailing `-N` replica index, so every surface hashes the same key.
///
/// Without this the promise above held only within one command: `ps` keys a
/// container as `web-1` while `images` keys the same service as `web`, so one
/// service came out in two different colours depending on which command printed
/// it. Measured before the fix, one project, one invocation: `migrate` was blue
/// in `ps` and light magenta in `images`.
///
/// Only an all-digit suffix is stripped, so a service genuinely named `api-2`
/// keeps its own identity rather than colliding with `api`.
fn strip_replica_suffix(key: &str) -> &str {
	match key.rsplit_once('-') {
		Some((head, tail))
			if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) =>
		{
			head
		}
		_ => key,
	}
}

/// Enable or disable user-facing lifecycle progress output process-wide. The CLI
/// enables it for the lifecycle commands; embedders that want podup silent leave
/// it off (the default).
pub fn set_progress(enabled: bool) {
	PROGRESS_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Whether user-facing lifecycle progress output is currently enabled.
pub fn progress_enabled() -> bool {
	PROGRESS_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Emit a concise per-resource lifecycle progress line to stderr, mirroring
/// docker compose's `Container <name>  <Action>` progress stream (which also
/// goes to stderr). `kind` is the resource noun (`Container`, `Network`,
/// `Volume`, `Image`); `action` is the past-tense verb (`Started`, `Removed`,
/// …), tinted green on a colour sink. A no-op unless [`set_progress`] enabled
/// progress output, so stdout consumers and machine output are never polluted.
pub fn progress_line(kind: &str, name: &str, action: &str) {
	use std::io::Write;
	if !progress_enabled() {
		return;
	}
	let verb = action_style(action);
	let ident = identity_style(name);
	// anstream::stderr strips the ANSI codes itself when colour is off.
	let _ = writeln!(
		anstream::stderr(),
		" {kind} {}{name}{}  {}{action}{}",
		ident.render(),
		ident.render_reset(),
		verb.render(),
		verb.render_reset()
	);
}

/// The colour band for a lifecycle verb.
///
/// Every verb used to be the same green, so `Volume data Removed` — which
/// destroys data and cannot be undone — was styled exactly like `Started`. The
/// bands say what kind of thing happened: something now exists (green),
/// something stopped but survives (yellow), something is gone (red), nothing
/// changed (dim).
fn action_style(action: &str) -> Style {
	let a = action.to_ascii_lowercase();
	// `unpaused` is checked before `paus` on purpose. It reaches the green arm
	// either way — a container resuming is a thing becoming active, which is what
	// green means here — but only by falling through, and adding `unpause` to the
	// yellow arm's prefixes would silently invert it. Naming it pins the intent.
	if a.starts_with("unpaus") {
		Style::new().fg_color(Some(AnsiColor::Green.into()))
	} else if a.starts_with("remov") || a.starts_with("kill") || a.starts_with("delet") {
		Style::new().fg_color(Some(AnsiColor::Red.into()))
	} else if a.starts_with("stop") || a.starts_with("paus") || a.starts_with("restart") {
		Style::new().fg_color(Some(AnsiColor::Yellow.into()))
	} else if a.starts_with("exist") || a.starts_with("running") || a.starts_with("skip") {
		Style::new().dimmed()
	} else {
		Style::new().fg_color(Some(AnsiColor::Green.into()))
	}
}

/// Emit a plain user-facing progress note to stderr (e.g. a "nothing to do"
/// line). A no-op unless [`set_progress`] enabled progress output.
pub fn progress_note(msg: &str) {
	use std::io::Write;
	if !progress_enabled() {
		return;
	}
	let _ = writeln!(anstream::stderr(), "{msg}");
}

/// Print a result line to stdout — used by `run -d` to echo the started
/// container's name so scripts capturing stdout get the id, matching
/// `docker compose run -d`. A no-op unless [`set_progress`] enabled progress
/// output, so embedders and machine paths stay silent.
///
/// The result line is the one thing scripts capture from stdout, so a failed
/// write is a real error (exit non-zero), not something to swallow — except a
/// broken pipe, which follows the process-wide quiet-exit convention.
pub fn result_line(msg: &str) -> std::io::Result<()> {
	use std::io::Write;
	if !progress_enabled() {
		return Ok(());
	}
	match writeln!(anstream::stdout(), "{msg}") {
		Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e),
		_ => Ok(()),
	}
}

/// Print a `label: value` line where the label is scaffolding and the value
/// carries the meaning.
///
/// `autostart status` is the densest meaning-per-line surface in the CLI — six
/// consecutive yes/no answers — and it was entirely monochrome, so finding the
/// one line that answers "is it running?" meant reading all six. The label is
/// dimmed and the value tinted by its own status meaning, which covers systemd's
/// vocabulary as well as Podman's.
///
/// Values with no status meaning (a path, a file mode) are left alone rather
/// than given an arbitrary colour.
pub fn print_labelled(label: &str, value: &str) {
	print_labelled_with(label, value, None);
}

/// [`print_labelled`] with the value's meaning stated rather than inferred.
///
/// Some values are prose, not a state word — `XDG_RUNTIME_DIR unset (systemctl
/// --user needs a user session)` is the answer to a yes/no question written as a
/// sentence. `Some(true)`/`Some(false)` colours it green/red; `None` falls back
/// to reading the text.
pub fn print_labelled_with(label: &str, value: &str, good: Option<bool>) {
	use std::io::Write;
	let dim = Style::new().dimmed();
	let padded = format!("{label}:");
	let explicit = good.map(|ok| {
		Style::new().fg_color(Some(
			if ok { AnsiColor::Green } else { AnsiColor::Red }.into(),
		))
	});
	let styled_value = match explicit.or_else(|| status_style(value)) {
		Some(style) => paint(style, value, true),
		None => value.to_string(),
	};
	let _ = writeln!(
		anstream::stdout(),
		"{}{padded:<11}{} {styled_value}",
		dim.render(),
		dim.render_reset()
	);
}

/// Bold — table headers and emphasis.
pub fn bold() -> Style {
	Style::new().bold()
}

/// Print `cols` as a bold table header through an anstream sink, so the styling
/// is stripped automatically when colour is off. The single place every list
/// command renders its header, keeping them consistent.
pub fn print_bold_header(cols: &str) {
	use std::io::Write;
	let s = bold();
	let _ = writeln!(
		anstream::stdout(),
		"{}{cols}{}",
		s.render(),
		s.render_reset()
	);
}

/// Print a bold header tinted with its identity colour — used where a block is
/// headed by a container name rather than by column titles.
pub fn print_identity_header(name: &str) {
	use std::io::Write;
	let style = identity_style(name).bold();
	let _ = writeln!(
		anstream::stdout(),
		"{}{name}{}",
		style.render(),
		style.render_reset()
	);
}

/// Bold red — the `error:` label.
pub fn error_style() -> Style {
	Style::new().bold().fg_color(Some(AnsiColor::Red.into()))
}

/// Wrap `text` in `style` when `enabled`, else return it unchanged. For raw sinks
/// that do not strip; anstream sinks should pass the codes through directly.
pub fn paint(style: Style, text: &str, enabled: bool) -> String {
	if enabled {
		format!("{}{text}{}", style.render(), style.render_reset())
	} else {
		text.to_string()
	}
}

/// Identity colours cycled through for per-service log prefixes. Deliberately
/// excludes red/green/yellow, which are reserved for status/severity meaning, so
/// a service prefix is never mistaken for an error/ok/warning signal.
const SERVICE_PALETTE: [AnsiColor; 6] = [
	AnsiColor::Cyan,
	AnsiColor::Magenta,
	AnsiColor::Blue,
	AnsiColor::BrightCyan,
	AnsiColor::BrightMagenta,
	AnsiColor::BrightBlue,
];

/// Stable palette index for a service name — the same name always maps to the
/// same colour (FNV-1a over the bytes, modulo the palette size).
fn palette_index(name: &str) -> usize {
	let mut h: u64 = 0xcbf2_9ce4_8422_2325;
	for b in name.bytes() {
		h ^= u64::from(b);
		h = h.wrapping_mul(0x0100_0000_01b3);
	}
	(h % SERVICE_PALETTE.len() as u64) as usize
}

/// The stable colour for a service's aggregated-log prefix.
pub fn service_style(name: &str) -> Style {
	Style::new().fg_color(Some(SERVICE_PALETTE[palette_index(name)].into()))
}

/// Whether an `Exited (N)` / `exited(N)` label reports a clean finish.
///
/// A container that ran to completion is not a failure, and colouring it red
/// says it is. One-shot services — migrations, seeds, a `command` that simply
/// ends — spend their whole life in this state, so red here is not a rare
/// cosmetic slip but the normal reading of a healthy project.
fn is_clean_exit(lower: &str) -> bool {
	// `exited (0)` and `exited(0)`, but not `exited (07)` or `exited (10)`.
	let Some(rest) = lower
		.split_once("exit")
		.map(|(_, r)| r.trim_start_matches(['e', 'd', ' ', '(']))
	else {
		return false;
	};
	rest.starts_with('0')
		&& !rest
			.trim_start_matches('0')
			.starts_with(|c: char| c.is_ascii_digit())
}

/// The semantic colour for a container status word, or `None` for an unknown one
/// (left uncoloured). Green = up/healthy, red = failed/dead/unhealthy, yellow =
/// paused/(re)starting/stopping, dim = created or finished cleanly. Matches on
/// substrings so it handles the bare state (`running`), Podman's verbose
/// `Status` (`Up 2 minutes`, `Exited (1)`), and systemd's vocabulary, which
/// `autostart status` reports.
fn status_style(status: &str) -> Option<Style> {
	let s = status.to_ascii_lowercase();
	// Checked before the red arm: `Exited (0)` contains "exit" and would
	// otherwise be indistinguishable from `Exited (7)`.
	if s.contains("exit") && is_clean_exit(&s) {
		return Some(Style::new().dimmed());
	}
	let colour = if s.contains("unhealthy")
		|| s.contains("exit")
		|| s.contains("dead")
		|| s.contains("failed")
		|| s.contains("not-found")
		|| s.contains("error")
	{
		AnsiColor::Red
	} else if s.contains("running")
		|| s.contains("healthy")
		|| s.starts_with("up")
		|| s == "active"
		|| s == "enabled"
		|| s == "yes"
	{
		AnsiColor::Green
	} else if s.contains("paus")
		|| s.contains("restart")
		|| s.contains("starting")
		|| s.contains("stopping")
		|| s.contains("removing")
		|| s.contains("activating")
	{
		AnsiColor::Yellow
	} else if s.contains("created") || s == "inactive" || s == "disabled" || s == "no" {
		return Some(Style::new().dimmed());
	} else {
		return None;
	};
	Some(Style::new().fg_color(Some(colour.into())))
}

/// The colour for an event action or a container state word, whichever matches.
///
/// `events` streams verbs (`start`, `die`, `kill`, `health_status`) that are not
/// container states but mean the same kinds of thing, so they resolve through
/// the lifecycle bands first and fall back to the status vocabulary.
pub fn action_or_status_style(word: &str) -> Option<Style> {
	let w = word.to_ascii_lowercase();
	if w.starts_with("die")
		|| w.starts_with("kill")
		|| w.starts_with("destroy")
		|| w.starts_with("remove")
	{
		return Some(Style::new().fg_color(Some(AnsiColor::Red.into())));
	}
	if w.starts_with("start") || w.starts_with("create") || w.starts_with("health") {
		return Some(Style::new().fg_color(Some(AnsiColor::Green.into())));
	}
	if w.starts_with("stop") || w.starts_with("pause") || w.starts_with("restart") {
		return Some(Style::new().fg_color(Some(AnsiColor::Yellow.into())));
	}
	status_style(word)
}

/// Colourise a status cell, tinting each comma-separated segment on its own.
///
/// `ls` reports a project as `running(1), exited(1)`, and styling that as one
/// string made the first matching substring win: `exit` came first, so a project
/// with a service up rendered **entirely red** — visually identical to one that
/// is completely dead. Splitting first means each state carries its own colour
/// and the mixed case reads as mixed.
///
/// Trailing padding is preserved verbatim so column alignment is untouched.
pub(crate) fn paint_status_cell(padded: &str) -> String {
	let trimmed = padded.trim_end();
	let pad = &padded[trimmed.len()..];
	let body = trimmed
		.split(", ")
		.map(|seg| match status_style(seg) {
			Some(style) => paint(style, seg, true),
			None => seg.to_string(),
		})
		.collect::<Vec<_>>()
		.join(", ");
	format!("{body}{pad}")
}

/// Render a container `status` left-padded to `width`, colourised by its meaning
/// when stdout is a colour sink. The padding is applied first so the colour codes
/// (zero display width) never disturb column alignment.
pub fn status_cell(status: &str, width: usize) -> String {
	let padded = format!("{status:<width$}");
	match status_style(status) {
		Some(style) => paint(style, &padded, stdout_colored()),
		None => padded,
	}
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
