//! Unit tests for the `ui` styling surface — split out to keep the module
//! within the source line limit, the same `tests.rs` split `autostart`, the
//! libpod client and `stats` already use.

use super::*;
use std::collections::HashMap;

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

/// The verb bands are what colour a progress line as it closes. A row that
/// closes with `Failed` previously fell through to the default green — the same
/// default that paints `Created`, so a failed row read as a successful one
/// except for the word. The fix is one prefix on the existing red arm (#1347).
#[test]
fn a_failed_progress_verb_is_red() {
	let red = Style::new().fg_color(Some(AnsiColor::Red.into()));
	let failed = action_style("Failed");
	let failed_lower = action_style("failed");
	assert_eq!(
		failed.render().to_string(),
		red.render().to_string(),
		"Failed must share the red band"
	);
	assert_eq!(
		failed_lower.render().to_string(),
		red.render().to_string(),
		"failed (lower) must share the red band"
	);
	// The verbs that were already red stay red — the change is additive.
	assert_eq!(
		action_style("Removed").render().to_string(),
		red.render().to_string()
	);
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

/// Serialises the tests that write the process-wide colour registry.
///
/// `SERVICES` is one static for the whole process, so two tests calling
/// `set_services` on different threads race and each can read the other's
/// registration. That would make both flaky, and a flaky test is a defect
/// rather than noise — so every test that writes the registry takes this
/// first. Tests that only build a local `HashMap` do not need it.
///
/// The lock is poison-tolerant on purpose: a panic in one of these tests must
/// fail that test, not cascade into every other one as a poisoned-lock panic
/// that hides which assertion actually broke.
static COLOUR_REGISTRY: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn registry_guard() -> std::sync::MutexGuard<'static, ()> {
	COLOUR_REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

/// `set_services` is what makes `service_style` disagree with the plain hash:
/// once a project's names are registered, sequential assignment guarantees
/// they spread across the palette, rather than each colliding independently
/// under the hash the way `palette_index` used to.
#[test]
fn set_services_makes_registered_names_distinct() {
	let _guard = registry_guard();
	// `set_services` is project-keyed; without a project name to register
	// under, every name falls back to the hash and the test's `assert_ne!`
	// chain no longer holds (#1517).
	set_project("colourreg-distinct");
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

/// Two projects in one process coexist: the first project's colours survive
/// the second project's registration.
///
/// Inversion of the defect pinned in #1518 — `SERVICES` was a single
/// process-wide static and `set_services` replaced it rather than merging,
/// so with two `Engine` values alive in one process (helmly-agent's shape,
/// a fleet of projects) the second registration dropped the first project's
/// names. They fell through to `colour_for`'s hash fallback and their colours
/// changed underneath a run that was still going, silently.
///
/// The registry is now keyed by project name: a second registration inserts
/// under a different project key, so the first project's slot stays put.
///
/// The two name sets are chosen so the registered slot and the hash fallback
/// disagree; the second assertion guards that the fixture can actually
/// discriminate — without it the first assertion would hold whether or not
/// the fix were in place, the vacuous shape this file already carries a
/// warning about elsewhere.
#[test]
fn a_second_project_leaves_the_first_projects_colours_alone() {
	let _guard = registry_guard();

	// `set_services` is project-keyed; each project's services live under its
	// own key (#1517). Register the first project's names under `evict-proj-a`.
	set_project("evict-proj-a");
	let first = ["evict-one".to_string(), "evict-two".to_string()];
	set_services(&first);
	let registered = service_slot("evict-one");

	// A second Engine registers its own project under a different key. With
	// project-keying, the first project's slot survives.
	set_project("evict-proj-b");
	set_services(&[
		"evict-other-alpha".to_string(),
		"evict-other-beta".to_string(),
	]);
	let after = service_slot("evict-one");

	// What the first project would get with no registry at all: the hash.
	let unregistered = crate::ui::palette::colour_for("evict-one", &HashMap::new());

	assert_eq!(
		after, registered,
		"the first project's slot must survive the second project's registration \
		 (#1517). If this now differs, the registry stopped being project-keyed \
		 - the first project's entries are being evicted again."
	);
	assert_ne!(
		registered, unregistered,
		"the fixture cannot discriminate: pick names whose registered slot and hash \
		 slot differ, or the assertion above holds whether or not eviction happened"
	);
}
