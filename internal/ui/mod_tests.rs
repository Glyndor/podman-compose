//! Unit tests for the `ui` styling surface — split out to keep the module
//! within the source line limit, the same `tests.rs` split `autostart`, the
//! libpod client and `stats` already use.

use super::*;

/// The bug this exists to stop: `ls` reports `running(1), exited(1)`, and
/// styling it as one string let the first matching substring win — `exit`
/// came first, so a project with a service up rendered entirely red,
/// indistinguishable from one that is completely dead.
#[test]
fn a_mixed_project_is_not_painted_as_one_state() {
	let out = paint_status_cell("running(1), exited(1)");
	let (running, exited) = out.split_once(", ").expect("both segments survive");
	assert_ne!(
		running.replace("running(1)", ""),
		exited.replace("exited(1)", ""),
		"each state must carry its own colour: {out:?}"
	);
}

/// A container that ran to completion is not a failure. One-shot services —
/// migrations, seeds, a `command` that simply ends — live in this state.
#[test]
fn a_clean_exit_is_not_red() {
	let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
	let clean = paint_status_cell("Exited (0)");
	assert!(
		!clean.contains(&red.render().to_string()),
		"a zero exit must not be red: {clean:?}"
	);
	let failed = paint_status_cell("Exited (7)");
	assert!(
		failed.contains(&red.render().to_string()),
		"a non-zero exit must stay red: {failed:?}"
	);
}

/// Digits after the first must not be mistaken for a clean exit.
#[test]
fn only_a_bare_zero_counts_as_clean() {
	assert!(is_clean_exit("exited (0)"));
	assert!(is_clean_exit("exited(0)"));
	assert!(!is_clean_exit("exited (10)"));
	assert!(!is_clean_exit("exited (07)"));
}

/// Padding is what keeps columns aligned, so colourising must not eat it.
#[test]
fn trailing_padding_survives_colourising() {
	let out = paint_status_cell("running   ");
	assert!(out.ends_with("   "), "{out:?}");
}

/// systemd's vocabulary reaches this through `autostart status`.
#[test]
fn systemd_states_are_coloured() {
	for word in ["active", "inactive", "failed", "not-found", "enabled"] {
		assert!(status_style(word).is_some(), "{word} should carry a colour");
	}
}

#[test]
fn paint_gates_on_enabled() {
	let plain = paint(bold(), "hi", false);
	assert_eq!(plain, "hi");
	let coloured = paint(bold(), "hi", true);
	assert!(coloured.contains("hi"));
	assert!(coloured.len() > "hi".len(), "enabled paint adds ANSI codes");
	assert!(coloured.starts_with('\u{1b}'), "starts with an ESC");
}

#[test]
fn colour_choice_resolution() {
	// Pure resolution — never touches the process-global choice, so it can't
	// race the production code (LinePrefixer/status_cell) that reads it.
	temp_env::with_var_unset("NO_COLOR", || {
		assert!(!colored_with(ColorChoice::Never, true));
		assert!(colored_with(ColorChoice::Always, false));
		assert!(colored_with(ColorChoice::Auto, true));
		assert!(!colored_with(ColorChoice::Auto, false));
	});
	// NO_COLOR forces plain in Auto, regardless of the TTY.
	temp_env::with_var("NO_COLOR", Some("1"), || {
		assert!(!colored_with(ColorChoice::Auto, true));
		// ...but an explicit `always` still overrides NO_COLOR.
		assert!(colored_with(ColorChoice::Always, true));
	});
}

#[test]
fn status_style_is_semantic() {
	assert_ne!(status_style("running"), status_style("exited (1)"));
	assert_ne!(status_style("unhealthy"), status_style("healthy"));
	assert!(status_style("Up 2 minutes").is_some());
	assert!(status_style("paused").is_some());
	assert!(status_style("created").is_some());
	assert!(status_style("weird-state").is_none());
}

#[test]
fn progress_toggle_is_observable() {
	// Off by default-or-restored; toggling flips the observable state. Restore
	// afterwards so the process-global flag does not leak into other tests.
	let prev = progress_enabled();
	set_progress(false);
	assert!(!progress_enabled());
	set_progress(true);
	assert!(progress_enabled());
	set_progress(prev);
}

#[test]
fn status_cell_pads_and_keeps_status() {
	let cell = status_cell("ok", 6);
	assert!(cell.contains("ok"));
	// At least the requested width (colour codes, if any, only add length).
	assert!(cell.len() >= 6);
}

/// The whole point of the shared key: `ps` prints `proj-web-1`, `logs`
/// prefixes `web-1`, and the progress lines print `proj-web-1` — all three
/// must resolve to one colour, or the palette is not stable at all.
#[test]
fn every_spelling_of_one_container_gets_one_colour() {
	set_project("proj");
	let from_ps = identity_style("proj-web-1");
	let from_logs = identity_style("web-1");
	assert_eq!(
		from_ps.render().to_string(),
		from_logs.render().to_string(),
		"the same container must be the same colour in ps and logs"
	);
}

/// A label that does not carry the project prefix is left alone.
#[test]
fn an_unprefixed_label_is_keyed_on_itself() {
	set_project("proj");
	assert_eq!(
		identity_style("web").render().to_string(),
		service_style("web").render().to_string()
	);
}

/// `set_services` is what makes `service_style` disagree with the plain hash:
/// once a project's names are registered, sequential assignment guarantees
/// they spread across the palette, rather than each colliding independently
/// under the hash the way `palette_index` used to.
#[test]
fn set_services_makes_registered_names_distinct() {
	set_services(&[
		"colourreg-alpha".to_string(),
		"colourreg-beta".to_string(),
		"colourreg-gamma".to_string(),
	]);
	let a = service_style("colourreg-alpha").render().to_string();
	let b = service_style("colourreg-beta").render().to_string();
	let g = service_style("colourreg-gamma").render().to_string();
	assert_ne!(a, b, "registered names must not share a colour");
	assert_ne!(b, g, "registered names must not share a colour");
	assert_ne!(a, g, "registered names must not share a colour");
}

/// The guard `palette_index("web") < SERVICE_PALETTE.len()` used to provide
/// before it was deleted: `service_style`'s narrow-terminal fallback receives
/// whatever slot `palette::assign` produced (0 through `WIDE_PALETTE.len() -
/// 1`, since `assign` itself wraps at the wide palette's size), and must wrap
/// that again to index the six-entry `SERVICE_PALETTE` safely.
///
/// Calls `slot_to_style` itself — the real narrow-branch code, not a
/// reimplementation of its modulo — for every slot the assignment can
/// produce, on both the wide and narrow branches. Confirmed this fails: with
/// the `% SERVICE_PALETTE.len()` removed from `slot_to_style`'s narrow arm,
/// this test panics with an index-out-of-bounds on slot 6 (`index out of
/// bounds: the len is 6 but the index is 6`), then passes again once the
/// modulo is restored.
#[test]
fn every_wide_palette_slot_indexes_the_narrow_palette_safely() {
	for slot in 0..palette::WIDE_PALETTE.len() {
		let _ = slot_to_style(slot, false);
		let _ = slot_to_style(slot, true);
	}
}
