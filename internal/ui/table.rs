//! Content-sized, truncating table rendering shared by the list commands
//! (`ls`, `ps`, `images`). Each column sizes to its widest cell (bounded by an
//! optional per-column cap, so a pathologically long value truncates with an
//! ellipsis instead of shoving every later column off its header), the way
//! `docker compose` lays out its tables. The trailing column is emitted raw
//! (never padded). The `--quiet` and `--format json` paths bypass this entirely.

/// Appended to a truncated cell to signal elision.
const ELLIPSIS: char = '…';

/// Fit `cell` into exactly `width` display columns: when it overflows, keep the
/// leading `width - 1` chars and append an ellipsis; otherwise left-pad with
/// spaces. Counts `char`s (not bytes) so multi-byte cells truncate on a char
/// boundary and stay aligned. A `width` of 0 returns the cell unchanged — used
/// for the trailing column, which is never padded or truncated.
pub fn fit_cell(cell: &str, width: usize) -> String {
	let cell = &sanitize_cell(cell);
	if width == 0 {
		return cell.to_string();
	}
	let len = cell.chars().count();
	if len <= width {
		return format!("{cell:<width$}");
	}
	let mut out: String = cell.chars().take(width - 1).collect();
	out.push(ELLIPSIS);
	out
}

/// Escape control characters so a cell cannot drive the terminal.
///
/// Cell contents are not ours: an image tag, a container name, a volume driver
/// and a process `argv` all come from outside podup. A raw `\x1b[` in one of
/// them repaints the caller's terminal, and — now that columns carry colour of
/// their own — desynchronises podup's own resets, so the rest of the table
/// inherits whatever the injected sequence set.
///
/// Escaping happens before padding, so the width the column reserves is the
/// width actually printed. Doing it after would let an escaped cell overflow its
/// column and break every row's alignment.
pub fn sanitize_cell(s: &str) -> String {
	s.chars()
		.flat_map(|c| {
			if c.is_control() {
				c.escape_default().collect::<Vec<_>>()
			} else {
				vec![c]
			}
		})
		.collect()
}

/// A list-command table whose columns size to their content (capped, so a
/// pathologically long cell truncates with an ellipsis rather than pushing every
/// later column past its header). The trailing column is emitted raw.
#[derive(Default)]
pub struct Table {
	headers: Vec<String>,
	/// Per-column max width; `None` sizes the column to its content unbounded.
	caps: Vec<Option<usize>>,
	/// The column (if any) whose cells are colourised by container status.
	status_col: Option<usize>,
	/// The column (if any) carrying an identity — a service or container name —
	/// tinted with that identity's stable colour.
	identity_col: Option<usize>,
	rows: Vec<Vec<String>>,
	/// Per-row identity key, parallel to `rows`. `None` falls back to the
	/// identity cell's own text.
	keys: Vec<Option<String>>,
}

impl Table {
	/// Start a table with the given column `headers`. Columns are uncapped (size
	/// to content) until bounded with [`Table::cap`].
	pub fn new(headers: &[&str]) -> Self {
		Self {
			headers: headers.iter().map(|h| (*h).to_string()).collect(),
			caps: vec![None; headers.len()],
			status_col: None,
			identity_col: None,
			rows: Vec::new(),
			keys: Vec::new(),
		}
	}

	/// Cap column `col` at `max` display columns; wider cells truncate with an
	/// ellipsis. The cap never shrinks a column below its header width.
	pub fn cap(mut self, col: usize, max: usize) -> Self {
		if let Some(slot) = self.caps.get_mut(col) {
			*slot = Some(max);
		}
		self
	}

	/// Mark column `col` as the status column, so its cells are colourised by
	/// meaning (green = up/healthy, red = exited/unhealthy, …) when stdout is a
	/// colour sink.
	pub fn status_col(mut self, col: usize) -> Self {
		self.status_col = Some(col);
		self
	}

	/// Tint column `col` with each row's stable identity colour, so the same
	/// service or container is the same colour in every command that lists it.
	///
	/// The palette deliberately excludes red, green and yellow — those carry
	/// status meaning — so an identity colour can never be misread as a state.
	pub fn identity_col(mut self, col: usize) -> Self {
		self.identity_col = Some(col);
		self
	}

	/// Append one data row. The cell count should match the header count; missing
	/// cells render blank and extra cells are ignored.
	pub fn push(&mut self, cells: Vec<String>) {
		self.rows.push(cells);
		self.keys.push(None);
	}

	/// Append a row whose identity colour is keyed on `key` rather than on the
	/// displayed cell.
	///
	/// The two differ where the column shows something longer than the identity:
	/// `ps` prints the full container name `proj-web-1` while `logs` prefixes the
	/// project-stripped `web-1`. Keying both on `web-1` is what makes one
	/// container the same colour in both commands — which is the entire point of
	/// a stable palette.
	pub fn push_keyed(&mut self, cells: Vec<String>, key: String) {
		self.rows.push(cells);
		self.keys.push(Some(key));
	}

	/// Content-sized width of each column: the widest of the header and its cells,
	/// bounded by the column cap (but never below the header width).
	fn widths(&self) -> Vec<usize> {
		self.headers
			.iter()
			.enumerate()
			.map(|(col, header)| {
				let header_w = header.chars().count();
				let content = self
					.rows
					.iter()
					.filter_map(|r| r.get(col))
					.map(|c| c.chars().count())
					.max()
					.unwrap_or(0);
				let mut w = header_w.max(content);
				if let Some(cap) = self.caps[col] {
					w = w.min(cap.max(header_w));
				}
				w
			})
			.collect()
	}

	/// Format one row's cells against the precomputed `widths`. The trailing
	/// column is emitted raw; when `colour` the status column is tinted by its
	/// meaning (the padding is applied first so the zero-width ANSI codes never
	/// disturb alignment).
	fn format_row(&self, cells: &[String], widths: &[usize], colour: bool) -> String {
		self.format_row_keyed(cells, widths, colour, None)
	}

	/// [`Table::format_row`] with the row's identity key, when it has one.
	fn format_row_keyed(
		&self,
		cells: &[String],
		widths: &[usize],
		colour: bool,
		key: Option<&str>,
	) -> String {
		let last = self.headers.len().saturating_sub(1);
		(0..self.headers.len())
			.map(|i| {
				let cell = cells.get(i).map(String::as_str).unwrap_or("");
				let w = if i == last { 0 } else { widths[i] };
				let padded = fit_cell(cell, w);
				if colour && Some(i) == self.status_col {
					return super::paint_status_cell(&padded);
				}
				if colour && Some(i) == self.identity_col && !cell.trim().is_empty() {
					// The padding is inside the paint so the colour does not stop
					// at the name and leave the gap bare; the codes are zero-width
					// either way, so alignment is untouched.
					return super::paint(super::identity_style(key.unwrap_or(cell)), &padded, true);
				}
				if colour && Some(i) == self.identity_col && !cell.trim().is_empty() {
					// The padding is inside the paint so the colour does not stop
					// at the name and leave the gap bare; the codes are zero-width
					// either way, so alignment is untouched.
					return super::paint(super::identity_style(key.unwrap_or(cell)), &padded, true);
				}
				padded
			})
			.collect::<Vec<_>>()
			.join(" ")
	}

	/// Render the table as plain (uncoloured) lines: the header first, then one
	/// line per row, columns aligned and over-cap cells truncated. Pure — used by
	/// the unit tests and shared with [`Table::print`].
	pub fn render(&self) -> Vec<String> {
		let widths = self.widths();
		let mut out = Vec::with_capacity(self.rows.len() + 1);
		out.push(self.format_row(&self.headers, &widths, false));
		for row in &self.rows {
			out.push(self.format_row(row, &widths, false));
		}
		out
	}

	/// Print the table to stdout: a bold header followed by the rows, with the
	/// status column (if any) colourised when stdout is a colour sink.
	pub fn print(&self) {
		let widths = self.widths();
		crate::ui::print_bold_header(&self.format_row(&self.headers, &widths, false));
		let colour =
			(self.status_col.is_some() || self.identity_col.is_some()) && super::stdout_colored();
		for (i, row) in self.rows.iter().enumerate() {
			let key = self.keys.get(i).and_then(Option::as_deref);
			println!("{}", self.format_row_keyed(row, &widths, colour, key));
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Cell contents come from outside podup — an image tag, a container name, a
	/// volume driver, a process argv. A raw escape sequence in one repaints the
	/// caller's terminal and desynchronises podup's own colour resets, so every
	/// row after it inherits whatever was injected.
	#[test]
	fn a_cell_cannot_drive_the_terminal() {
		let out = fit_cell("evil\x1b[31m\x07\tname", 0);
		assert!(!out.contains('\x1b'), "{out:?}");
		assert!(!out.contains('\x07'), "{out:?}");
		assert!(!out.contains('\t'), "{out:?}");
		assert!(out.contains("name"), "{out:?}");
	}

	/// Printable text is untouched.
	#[test]
	fn a_printable_cell_passes_through() {
		assert_eq!(fit_cell("proj_data-1", 0), "proj_data-1");
	}

	/// Escaping happens before padding, so the width a column reserves is the
	/// width actually printed — otherwise an escaped cell overflows its column
	/// and breaks alignment on every row.
	#[test]
	fn escaping_happens_before_padding() {
		// One control char escapes to two visible characters.
		assert_eq!(fit_cell("a\tb", 6).len(), 6);
	}

	#[test]
	fn fit_cell_pads_short_values_to_width() {
		assert_eq!(fit_cell("web", 6), "web   ");
		// Exactly the width is kept verbatim (padded to itself).
		assert_eq!(fit_cell("alpine", 6), "alpine");
	}

	#[test]
	fn fit_cell_truncates_with_an_ellipsis() {
		// One over the width: keep width-1 chars plus the ellipsis (display width
		// stays == width).
		let out = fit_cell("docker.io/library/alpine", 10);
		assert_eq!(out.chars().count(), 10);
		assert!(out.ends_with(ELLIPSIS));
		assert!(out.starts_with("docker.io"));
	}

	#[test]
	fn fit_cell_counts_chars_not_bytes() {
		// Multi-byte cell truncated on a char boundary, no panic, width honoured.
		let out = fit_cell("café-service-name", 6);
		assert_eq!(out.chars().count(), 6);
		assert!(out.ends_with(ELLIPSIS));
	}

	#[test]
	fn fit_cell_width_zero_returns_cell_unchanged() {
		assert_eq!(fit_cell("anything-at-all", 0), "anything-at-all");
		assert_eq!(fit_cell("", 0), "");
	}

	#[test]
	fn columns_size_to_their_widest_cell() {
		let mut t = Table::new(&["NAME", "STATUS"]);
		t.push(vec!["a-very-long-project-name".into(), "running(1)".into()]);
		t.push(vec!["x".into(), "exited(1)".into()]);
		let lines = t.render();
		// Header NAME column is padded to the widest name (24 chars), so STATUS
		// starts at the same offset on every line.
		let name_w = "a-very-long-project-name".chars().count();
		assert!(lines[0].starts_with(&format!("{:<width$} ", "NAME", width = name_w)));
		assert!(lines[2].starts_with(&format!("{:<width$} ", "x", width = name_w)));
		// Short content does not blow the column out to a fixed width.
		assert_eq!(name_w, 24);
	}

	#[test]
	fn over_cap_cells_truncate_and_stay_aligned() {
		let mut t = Table::new(&["NAME", "STATUS"]).cap(0, 10).status_col(1);
		t.push(vec!["this-name-is-far-too-long".into(), "running".into()]);
		t.push(vec!["short".into(), "exited".into()]);
		let lines = t.render();
		// Every line's first column occupies exactly the cap (10) before the gap,
		// so STATUS lands in the same place; the long name carries the ellipsis.
		for line in &lines {
			assert_eq!(
				line.chars().nth(10),
				Some(' '),
				"gap at the cap on {line:?}"
			);
		}
		assert!(lines[1].contains(ELLIPSIS));
	}

	#[test]
	fn cap_never_shrinks_below_the_header() {
		// A cap smaller than the header keeps the header intact (no truncation).
		let mut t = Table::new(&["REPOSITORY"]).cap(0, 3);
		t.push(vec!["x".into()]);
		let lines = t.render();
		assert_eq!(lines[0], "REPOSITORY");
	}

	#[test]
	fn trailing_column_is_emitted_raw() {
		// The last column is neither padded nor truncated (no later column to
		// misalign), even when much longer than its header.
		let mut t = Table::new(&["NAME", "PORTS"]).cap(0, 8);
		let ports = "0.0.0.0:8080->80/tcp, 0.0.0.0:8443->443/tcp";
		t.push(vec!["web".into(), ports.into()]);
		let lines = t.render();
		assert!(lines[1].ends_with(ports));
	}

	#[test]
	fn missing_cells_render_blank() {
		let mut t = Table::new(&["A", "B"]);
		t.push(vec!["only-a".into()]);
		let lines = t.render();
		// No panic; the absent B cell is blank (the line is the padded A plus a gap).
		assert!(lines[1].starts_with("only-a"));
	}
}
