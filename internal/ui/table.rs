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

/// The style for a [`Table::caution_col`] cell: yellow for the answer that
/// warrants a second look, dim for the default one, and nothing for a value that
/// is neither.
///
/// Pure and taking the cell text, so the mapping is unit-testable without
/// rendering a table or reading the process-global colour choice.
fn caution_style(cell: &str) -> super::Style {
	match cell.trim() {
		"yes" => super::Style::new().fg_color(Some(super::AnsiColor::Yellow.into())),
		"no" => super::Style::new().dimmed(),
		_ => super::Style::new(),
	}
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
	/// The column (if any) holding a yes/no answer where `yes` is the one worth
	/// noticing. See [`Table::caution_col`].
	caution_col: Option<usize>,
	/// Columns rendered dim, so the ones left at normal weight are what the eye
	/// lands on. See [`Table::dim_cols`].
	dim_cols: Vec<usize>,
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
			caution_col: None,
			dim_cols: Vec::new(),
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

	/// Mark column `col` as a yes/no answer where `yes` is the one worth noticing:
	/// `yes` takes the yellow band, `no` is dimmed as the unremarkable default.
	///
	/// Deliberately not [`Table::status_col`], which would paint `yes` green.
	/// Green means healthy/up everywhere else in this CLI, and the column this
	/// was written for — `volumes`' `EXTERNAL` — is not reporting health. It
	/// reports the one volume podup will refuse to delete, so a `down -v` that
	/// leaves something standing is explicable. That is a caution, and yellow is
	/// already the band this CLI uses for *survives*.
	pub fn caution_col(mut self, col: usize) -> Self {
		self.caution_col = Some(col);
		self
	}

	/// Dim the given columns, leaving the rest at normal weight.
	///
	/// For a table where most columns are scaffolding and one or two carry the
	/// answer. `top` is the case: eight columns of process bookkeeping around the
	/// command line, which is the reason anyone runs it. Dimming is not a
	/// meaning — unlike the status and caution colours it says nothing about the
	/// value — so it composes with them rather than competing.
	pub fn dim_cols(mut self, cols: &[usize]) -> Self {
		self.dim_cols = cols.to_vec();
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

	/// Whether any column marker is set, i.e. whether rendering with colour could
	/// differ from rendering without it.
	///
	/// Kept as one predicate so adding a marker is a single edit: the gate in
	/// [`Table::print`] and the marker list cannot drift apart.
	fn colours_any_column(&self) -> bool {
		self.status_col.is_some()
			|| self.identity_col.is_some()
			|| self.caution_col.is_some()
			|| !self.dim_cols.is_empty()
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
				if colour && Some(i) == self.caution_col {
					return super::paint(caution_style(cell), &padded, true);
				}
				if colour && self.dim_cols.contains(&i) {
					return super::paint(super::Style::new().dimmed(), &padded, true);
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
		// Every column marker that can tint a cell has to appear here, or a table
		// that uses only that marker renders plain. `caution_col` was added
		// without being listed, and it went unnoticed because its first caller —
		// `volumes` — also sets `identity_col`, so the gate happened to be open.
		let colour = self.colours_any_column() && super::stdout_colored();
		for (i, row) in self.rows.iter().enumerate() {
			let key = self.keys.get(i).and_then(Option::as_deref);
			println!("{}", self.format_row_keyed(row, &widths, colour, key));
		}
	}

	/// Whether the table has any data rows. Callers can use this to print an
	/// explicit "no X" line on stderr for an empty result (#1675), so a script
	/// capturing stdout sees nothing for the empty case.
	pub fn is_empty(&self) -> bool {
		self.rows.is_empty()
	}
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
