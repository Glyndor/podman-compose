use super::{prefix_slot, prefix_style, LinePrefixer};

/// The regression this guards: `new` used to resolve `service_slot(label)`
/// directly against the raw, replica-suffixed label, which is never a key
/// in the per-project registry `identity_slot` resolves against, so every log
/// prefix silently fell back to the hash instead of the sequential colour
/// `ps` uses for the same container.
///
/// Both checks are on `prefix_slot`, the actual routing decision, not the
/// `Style` [`prefix_style`] renders it into: the narrow (6-colour) fallback
/// wraps a slot index mod 6, and for these two labels slot 1 ("web") and
/// slot 19 ("web-1") both land on Magenta there. A `Style` comparison here
/// was red on any terminal that never announces the wide palette (no
/// `TERM`/`COLORTERM`, which is every Linux/macOS CI leg today) whether or
/// not the regression it guards was present. The slot itself never wraps,
/// so it stays a real, palette-independent assertion, and since
/// `prefix_style` does nothing but render `prefix_slot`'s output (see its
/// doc comment), pinning the slot pins the `Style` too.
#[test]
fn prefix_style_routes_through_identity_style_not_service_style() {
	assert_eq!(
		prefix_slot("web-1"),
		crate::ui::identity_slot("web"),
		"the replica suffix must be stripped before resolving the colour"
	);
	assert_ne!(
		prefix_slot("web-1"),
		crate::ui::service_slot("web-1"),
		"the raw, suffixed label must not be hashed directly"
	);
	// `prefix_style` is a pure rendering of `prefix_slot`, so its `Style`
	// still agrees with `identity_style` on a wide-palette terminal, kept
	// as a smoke test that the rendering step itself is wired up.
	assert_eq!(
		prefix_style("web-1"),
		crate::ui::identity_style("web"),
		"prefix_style must render the same slot identity_style does"
	);
}

/// #1082: one space before the bar. Attached `up` already used one, so the
/// same container was tagged two different ways by two commands in the same
/// binary; docker compose uses one too.
#[test]
fn line_prefixer_tags_lines_and_buffers_partials() {
	let mut p = LinePrefixer::new("web", true, false);
	let mut out: Vec<u8> = Vec::new();
	p.write(&mut out, b"hello\nwor").unwrap();
	// The complete line is tagged; the partial "wor" waits for its newline.
	assert_eq!(out, b"web | hello\n");
	p.write(&mut out, b"ld\n").unwrap();
	assert_eq!(out, b"web | hello\nweb | world\n");
}

#[test]
fn line_prefixer_flush_tail_emits_unterminated_line() {
	let mut p = LinePrefixer::new("db", true, false);
	let mut out: Vec<u8> = Vec::new();
	p.write(&mut out, b"partial").unwrap();
	assert!(out.is_empty(), "a line with no newline is held back");
	p.flush_tail(&mut out);
	assert_eq!(out, b"db | partial\n");
}

#[test]
fn line_prefixer_bounds_a_newlineless_flood() {
	use super::MAX_PENDING;
	let mut p = LinePrefixer::new("web", true, false);
	let mut out: Vec<u8> = Vec::new();
	// Feed more than the cap with no newline in sight, in small chunks.
	let chunk = vec![b'x'; 4096];
	for _ in 0..((MAX_PENDING / chunk.len()) + 2) {
		p.write(&mut out, &chunk).unwrap();
	}
	// The partial was flushed as a prefixed line instead of being buffered
	// unbounded, and nothing is left pending beyond the last sub-cap chunk.
	assert!(
		!out.is_empty(),
		"the over-long partial was emitted, not held"
	);
	assert!(
		p.pending.len() < MAX_PENDING,
		"pending stays bounded under the cap, was {}",
		p.pending.len()
	);
	assert!(
		out.starts_with(b"web | "),
		"the flushed partial is prefixed"
	);
}

#[test]
fn line_prefixer_no_prefix_emits_bare_lines() {
	// `--no-log-prefix`: lines pass through with no `{label} | ` tag.
	let mut p = LinePrefixer::new("web", false, false);
	let mut out: Vec<u8> = Vec::new();
	p.write(&mut out, b"hello\n").unwrap();
	assert_eq!(out, b"hello\n");
	p.write(&mut out, b"tail").unwrap();
	p.flush_tail(&mut out);
	assert_eq!(out, b"hello\ntail\n");
}

/// #1102: a sink that has gone away must surface as an error, not be
/// swallowed. Every write here used to be `let _ =`, so `logs -f | head`
/// streamed into a closed pipe forever instead of exiting.
#[test]
fn line_prefixer_surfaces_a_broken_pipe() {
	/// A writer that refuses everything the way a closed pipe does.
	struct ClosedPipe;
	impl std::io::Write for ClosedPipe {
		fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
			Err(std::io::Error::new(
				std::io::ErrorKind::BrokenPipe,
				"broken pipe",
			))
		}
		fn flush(&mut self) -> std::io::Result<()> {
			Ok(())
		}
	}

	let mut p = LinePrefixer::new("web", true, false);
	let err = p
		.write(&mut ClosedPipe, b"hello\n")
		.expect_err("a closed sink must be reported");
	assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
}
