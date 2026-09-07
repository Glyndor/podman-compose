//! `podup audit`: render hardening findings for every service in a parsed
//! compose file. The command is a read-only view; it never contacts Podman
//! and never changes what `up` does.
//!
//! Checks live as a flat list of pure functions over the parsed
//! [`podup::compose::types::Service`]. Each check returns the list of
//! [`Finding`]s it raised; [`audit_file`] walks the file and the
//! [`render_table`]/[`render_json`] functions translate the result into the
//! `--format table|json` output the CLI asked for.

use std::io::{self, Write};

use podup::compose::types::{ComposeFile, Service};

mod checks;

/// One detected hardening finding: a service that did something the spec
/// asked us to flag, identified by a stable snake_case id the caller can grep
/// (`podup audit --format json` is machine-readable specifically so this id
/// is stable across releases).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
	/// Service name (the compose key, not the container name). Stable
	/// across reloads; the audit table sorts by this.
	pub service: String,
	/// Stable snake_case check id (`privileged`, `host_namespace`, …).
	pub check: &'static str,
	/// One-line human-readable explanation. Printed after the table row and
	/// emitted as the `reason` field in `--format json`.
	pub reason: String,
}

/// What the audit walked: the file's services sorted by service name, and
/// the findings keyed by the (service, check) pairs they belong to.
#[derive(Debug, Default)]
pub struct AuditReport {
	/// Findings, in the order the checks produced them.
	pub findings: Vec<Finding>,
}

impl AuditReport {
	/// Whether at least one finding was produced.
	pub fn has_findings(&self) -> bool {
		!self.findings.is_empty()
	}

	/// Group every finding by service name, preserving the service order
	/// ([`audit_file`] visits the services in YAML order). The findings
	/// inside each group stay as refs into `self` so the renderer can pull
	/// the check id and reason without cloning.
	fn by_service<'a, 'b>(
		&'a self,
		services: &'b [(&str, &Service)],
	) -> Vec<(&'b str, Vec<&'a Finding>)> {
		let mut out = Vec::with_capacity(services.len());
		for (name, _) in services {
			let mine: Vec<&'a Finding> = self
				.findings
				.iter()
				.filter(|f| f.service == *name)
				.collect();
			out.push((*name, mine));
		}
		out
	}
}

/// Run every check against every service in `file`, returning the
/// accumulated findings. Services are visited in the order they appear in
/// [`ComposeFile::services`] (insertion order, which the parser preserves from
/// the YAML). A service with no findings contributes nothing to the report.
///
/// Pure: no I/O, no logging, no global state. The callers wire the result
/// into whichever renderer matches `--format`.
pub fn audit_file(file: &ComposeFile) -> AuditReport {
	let mut report = AuditReport::default();
	for (name, service) in &file.services {
		let findings = checks::run_checks(name, service, file);
		report.findings.extend(findings);
	}
	report
}

/// Snapshot of `file.services` as an ordered slice of `(name, &service)`
/// pairs in YAML order. Used by the table renderer so every row is in the
/// same order the YAML declared (insertion order in the underlying
/// `IndexMap`); `IndexMap::iter` already gives that, the slice form just
/// keeps the borrow checker happy at the call site.
pub fn ordered_services(file: &ComposeFile) -> Vec<(&str, &Service)> {
	file.services.iter().map(|(k, v)| (k.as_str(), v)).collect()
}

/// Render `report` for `services` in `table` form to stdout.
///
/// Layout: header `SERVICE FINDINGS`, one row per service with the check ids
/// joined by single spaces (or `-` when there are none), then one line per
/// finding with `  <service>: <check>: <reason>` indented two spaces. When
/// nothing was found, only the line `no findings` is printed, the table is
/// suppressed so a CI log without any findings is minimal.
///
/// Service names and reasons are user-controlled (compose YAML), so the
/// detail lines route them through `sanitize_cell` before printing; the
/// table itself already does that via `Table::print`. Without sanitization
/// a malicious compose file injects raw ESC bytes into the operator's
/// terminal, and the bytes survive a pipe because `println!` is just a
/// byte stream.
pub fn render_table(services: &[(&str, &Service)], report: &AuditReport) {
	let stdout = io::stdout();
	let mut out = stdout.lock();
	let _ = render_table_to(&mut out, services, report);
}

/// [`render_table`] writing to `w`, factored out so tests can capture the
/// rendered bytes without redirecting the process's stdout. The sanitization
/// of user-controlled values happens here, not in the caller, so every sink
/// of audit output goes through the same gate.
pub fn render_table_to<W: Write>(
	w: &mut W,
	services: &[(&str, &Service)],
	report: &AuditReport,
) -> io::Result<()> {
	let mut table = podup::ui::Table::new(&["SERVICE", "FINDINGS"]);
	for (name, mine) in report.by_service(services) {
		let findings = if mine.is_empty() {
			"-".to_string()
		} else {
			mine.iter().map(|f| f.check).collect::<Vec<_>>().join(" ")
		};
		table.push(vec![(*name).to_string(), findings]);
	}
	if report.findings.is_empty() {
		writeln!(w, "no findings")?;
		return Ok(());
	}
	table.print_to(w)?;
	for finding in &report.findings {
		// `check` is a hardcoded `'static str`, so only the user-controlled
		// `service` and `reason` need sanitizing. Going through `sanitize_cell`
		// keeps the table cell and detail line consistent: same function,
		// same rules, no drift between the two surfaces an attacker can read.
		writeln!(
			w,
			"  {}: {}: {}",
			podup::ui::sanitize_cell(&finding.service),
			finding.check,
			podup::ui::sanitize_cell(&finding.reason),
		)?;
	}
	Ok(())
}

/// Render `report` as a single JSON line to stdout. `serde_json` orders the
/// object keys alphabetically (`findings`, then per-finding `check`, `reason`,
/// `service`), which gives the stable shape CI scripts can rely on.
///
/// Empty finding lists emit `{"findings":[]}`, never `null`, so an empty
/// body is still valid JSON without the consumer having to special-case it.
pub fn render_json(report: &AuditReport) {
	let stdout = io::stdout();
	let mut out = stdout.lock();
	render_json_to(&mut out, report).expect("write to stdout");
}

/// [`render_json`] writing to `w`. JSON output is already safe from terminal
/// injection: `serde_json` escapes every control character (`\x1b` becomes
/// `\u001b`), so a malicious compose file's bytes round-trip as escape
/// sequences inside a JSON string rather than as live escapes on the
/// terminal. The sink is exposed so a test can compare the exact JSON it
/// produces against the same input.
pub fn render_json_to<W: Write>(w: &mut W, report: &AuditReport) -> io::Result<()> {
	let findings: Vec<serde_json::Value> = report
		.findings
		.iter()
		.map(|f| {
			serde_json::json!({
				"check": f.check,
				"reason": f.reason,
				"service": f.service,
			})
		})
		.collect();
	let out = serde_json::json!({ "findings": findings });
	writeln!(w, "{out}")
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
