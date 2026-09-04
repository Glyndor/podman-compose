use super::size_cells;
use crate::libpod::types::volume::VolumeDiskUsage;

fn usage(size: u64, reclaimable: u64) -> VolumeDiskUsage {
	VolumeDiskUsage {
		name: "vol".into(),
		size,
		reclaimable,
		links: 1,
	}
}

/// The byte counts `system/df` really reported on this host, rendered the way
/// podup renders every other size: decimal, three significant digits.
///
/// `podman system df -v` prints four (`193.2MB`, `67.33MB`, measured 2026-08-03)
/// while `podman images` and `podman ps -s` print three. podman is not
/// self-consistent, so matching podup's own columns wins ,  recorded here so the
/// divergence is a decision rather than an accident somebody later "fixes".
#[test]
fn the_size_cells_render_at_three_significant_digits() {
	assert_eq!(size_cells(Some(&usage(193_243_902, 0))).0, "193MB");
	assert_eq!(size_cells(Some(&usage(67_331_781, 0))).0, "67.3MB");
	assert_eq!(size_cells(Some(&usage(0, 0))).0, "0B");
}

/// A volume the host accounting never mentions renders empty, not `0B`.
///
/// A compose file can declare a volume that has never been created, and libpod
/// only reports what exists. An empty cell says it is not there; `0B` would
/// claim it exists and holds nothing ,  two different answers a reader would act
/// on differently.
#[test]
fn a_volume_that_does_not_exist_yet_renders_empty() {
	assert_eq!(size_cells(None), (String::new(), String::new()));
}

/// A volume that exists and is genuinely empty renders `0B`, so the blank above
/// is keyed on absence and not on size.
#[test]
fn an_existing_empty_volume_renders_zero() {
	assert_eq!(size_cells(Some(&usage(0, 0))), ("0B".into(), "0B".into()));
}

/// SIZE and RECLAIMABLE are different numbers and both are shown. A volume a
/// container still links reports its full size and zero reclaimable, which is
/// the fact someone clearing disk space needs ,  collapsing the two would hide
/// exactly the case worth seeing.
#[test]
fn reclaimable_is_reported_separately_from_size() {
	let linked = size_cells(Some(&usage(193_243_902, 0)));
	let unlinked = size_cells(Some(&usage(193_243_902, 193_243_902)));
	assert_eq!(linked, ("193MB".into(), "0B".into()));
	assert_eq!(unlinked, ("193MB".into(), "193MB".into()));
}

/// An empty `volumes` table prints one explicit line on stderr instead of the
/// bare header. Without it a script parsing stdout (`volumes | awk …`) sees
/// just a header on stdout for an empty project, and the header is the only
/// line ,  which a parser distinguishes from "no volumes" only by row count.
/// Stdout stays empty; stderr carries the explicit `no volumes` (#1675).
///
/// The full `Engine::list_volumes` path needs an `Engine` and a `ComposeFile`,
/// neither of which is light to construct for a unit test. Instead, pin the
/// table's empty predicate (the branch the printer reads) and the source path
/// that consults it. A regression that drops the `table.is_empty()` branch
/// from `list_volumes` would make the next header-only line appear on stdout
/// again, and this test would catch it.
#[test]
fn an_empty_volumes_table_prints_one_line_on_stderr() {
	let mut t = crate::ui::Table::new(&["NAME", "DRIVER", "EXTERNAL"]);
	assert!(t.is_empty(), "a freshly built table is empty");
	t.push(vec!["data".into(), "local".into(), "no".into()]);
	assert!(!t.is_empty(), "a table with one row is not empty");
	// The empty-table branch in `list_volumes` consults `Table::is_empty()`
	// before printing, and the regression that would matter is removing the
	// call to `crate::ui::progress_note("no volumes")` in that branch.
	let src = include_str!("list.rs");
	assert!(
		src.contains("table.is_empty()"),
		"the volumes printer must gate its empty case on Table::is_empty: {src}"
	);
	assert!(
		src.contains("progress_note(\"no volumes\")"),
		"the empty volumes path must print `no volumes` on stderr via progress_note: {src}"
	);
}
