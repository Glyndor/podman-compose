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

/// A caution column separates its two answers, and `yes` is not the same as
/// `no` with a different word. Asserted as "the two styles differ" rather
/// than against a literal escape sequence: re-deriving the expected code from
/// the same constant the renderer reads would pass whether or not the
/// renderer consulted it.
#[test]
fn caution_style_distinguishes_yes_from_no() {
	assert_ne!(caution_style("yes"), caution_style("no"));
	// Padding must not change the answer — cells reach it already padded.
	assert_eq!(caution_style("yes  "), caution_style("yes"));
}

/// A value that is neither answer is left alone rather than given an
/// arbitrary colour, matching how `status_style` treats an unknown word.
#[test]
fn caution_style_leaves_an_unknown_value_alone() {
	assert_eq!(caution_style("maybe"), super::super::Style::new());
}

/// The caution column reaches the rendered row. `render` is the uncoloured
/// path, so this pins that the column is *declared*; the colour itself is
/// asserted on `caution_style` above.
#[test]
fn caution_col_survives_rendering() {
	let mut t = Table::new(&["NAME", "EXTERNAL"]).caution_col(1);
	t.push(vec!["theirs".into(), "yes".into()]);
	let rows = t.render();
	assert!(rows[1].contains("yes"), "{rows:?}");
}

/// A table whose only marker is `caution_col` still colours. The gate in
/// `print` listed `status_col` and `identity_col` only, and the first caution
/// caller set `identity_col` too — so the omission was invisible.
#[test]
fn a_caution_only_table_still_colours() {
	let t = Table::new(&["NAME", "EXTERNAL"]).caution_col(1);
	assert!(t.colours_any_column());
}

/// Dimming is a column property, so it reaches the gate the same way the
/// meaning-carrying markers do.
#[test]
fn a_dim_only_table_still_colours() {
	let t = Table::new(&["PID", "CMD"]).dim_cols(&[0]);
	assert!(t.colours_any_column());
	assert!(!Table::new(&["PID", "CMD"]).colours_any_column());
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
