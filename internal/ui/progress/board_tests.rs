use super::*;

fn t0() -> Instant {
	Instant::now()
}

fn seeded() -> Board {
	Board::new([
		(Kind::Network, "proj_default".to_string()),
		(Kind::Container, "proj-web-1".to_string()),
		(Kind::Container, "proj-db-1".to_string()),
	])
}

/// The whole point of seeding is that the board shows what is still to come, so
/// a fresh board already knows its total.
#[test]
fn a_seeded_board_knows_its_total_before_anything_happens() {
	assert_eq!(seeded().tally(), (0, 3));
}

/// The counter tracks finished rows, not started ones. A row being worked on is
/// not progress the user can rely on.
#[test]
fn only_finished_rows_count_towards_the_tally() {
	let mut b = seeded();
	let now = t0();
	b.start(Kind::Container, "proj-web-1", "Creating", now);
	assert_eq!(b.tally(), (0, 3));
	b.finish(Kind::Container, "proj-web-1", "Created", now);
	assert_eq!(b.tally(), (1, 3));
}

/// Most of the existing call sites report only an ending. The board has to
/// accept that rather than requiring a matching start it will never get.
#[test]
fn a_finish_without_a_start_still_lands() {
	let mut b = seeded();
	b.finish(Kind::Network, "proj_default", "Created", t0());
	assert_eq!(b.tally(), (1, 3));
}

/// A resource the seed did not predict (an implicit network, a `--scale`
/// override) is appended rather than dropped. Losing the row is worse than a
/// board that grows by one.
#[test]
fn an_unseeded_resource_is_appended_not_dropped() {
	let mut b = seeded();
	b.finish(Kind::Volume, "proj_data", "Created", t0());
	assert_eq!(b.tally(), (1, 4));
}

/// Kind and name together identify a row: a network and a container may share a
/// name without sharing a row.
#[test]
fn a_row_is_identified_by_kind_and_name_together() {
	let mut b = Board::new([
		(Kind::Network, "same".to_string()),
		(Kind::Container, "same".to_string()),
	]);
	b.finish(Kind::Network, "same", "Created", t0());
	assert_eq!(b.tally(), (1, 2));
}

/// Completed rows leave the live region in order, so the permanent history
/// reads in the order the work happened.
#[test]
fn completed_rows_flush_from_the_front_in_order() {
	let mut b = seeded();
	let now = t0();
	b.finish(Kind::Network, "proj_default", "Created", now);
	let flushed = b.take_completed_prefix();
	assert_eq!(flushed.len(), 1);
	assert_eq!(flushed[0].name, "proj_default");
	assert_eq!(b.live_rows().len(), 2);
}

/// A finished row behind an unfinished one stays put. Flushing it would print
/// the permanent history out of order, and that record is what `up` leaves
/// behind.
#[test]
fn a_finished_row_behind_an_unfinished_one_does_not_flush() {
	let mut b = seeded();
	let now = t0();
	b.finish(Kind::Container, "proj-db-1", "Created", now);
	assert!(b.take_completed_prefix().is_empty());
	assert_eq!(b.live_rows().len(), 3);

	// Once the rows ahead of it finish, the whole run leaves together.
	b.finish(Kind::Network, "proj_default", "Created", now);
	b.finish(Kind::Container, "proj-web-1", "Created", now);
	let flushed = b.take_completed_prefix();
	assert_eq!(flushed.len(), 3);
	assert!(b.live_rows().is_empty());
}

/// A row is never flushed twice, however often the renderer asks.
#[test]
fn flushing_is_not_repeatable() {
	let mut b = seeded();
	b.finish(Kind::Network, "proj_default", "Created", t0());
	assert_eq!(b.take_completed_prefix().len(), 1);
	assert!(b.take_completed_prefix().is_empty());
}

/// A finished row stops counting up. Without freezing the elapsed time, a
/// completed row would keep ticking for as long as the command runs.
#[test]
fn a_finished_row_freezes_its_elapsed_time() {
	let mut b = seeded();
	let start = t0();
	b.start(Kind::Container, "proj-web-1", "Creating", start);
	let end = start + Duration::from_millis(250);
	b.finish(Kind::Container, "proj-web-1", "Created", end);
	let row = b
		.live_rows()
		.iter()
		.find(|r| r.name == "proj-web-1")
		.unwrap()
		.clone();
	let much_later = start + Duration::from_secs(30);
	assert_eq!(row.duration(much_later), Some(Duration::from_millis(250)));
}

/// A working row counts up from when it started, not from when the board did.
#[test]
fn a_working_row_counts_from_its_own_start() {
	let mut b = seeded();
	let t = t0();
	b.start(
		Kind::Container,
		"proj-web-1",
		"Creating",
		t + Duration::from_secs(2),
	);
	let row = b.live_rows()[1].clone();
	assert_eq!(
		row.duration(t + Duration::from_secs(5)),
		Some(Duration::from_secs(3))
	);
}

/// A pending row shows no time, because nothing has taken any.
#[test]
fn a_pending_row_has_no_duration() {
	let b = seeded();
	assert_eq!(b.live_rows()[0].duration(t0()), None);
}

/// Re-starting a row that is already working keeps its original start, so a
/// retry does not reset the clock and hide how long it really took.
#[test]
fn restarting_a_working_row_keeps_the_original_start() {
	let mut b = seeded();
	let t = t0();
	b.start(Kind::Container, "proj-web-1", "Creating", t);
	b.start(
		Kind::Container,
		"proj-web-1",
		"Starting",
		t + Duration::from_secs(4),
	);
	let row = b.live_rows()[1].clone();
	assert_eq!(
		row.duration(t + Duration::from_secs(6)),
		Some(Duration::from_secs(6))
	);
}

#[test]
fn completion_is_every_row_finished() {
	let mut b = seeded();
	let now = t0();
	assert!(!b.is_complete());
	for (kind, name) in [
		(Kind::Network, "proj_default"),
		(Kind::Container, "proj-web-1"),
		(Kind::Container, "proj-db-1"),
	] {
		b.finish(kind, name, "Created", now);
	}
	assert!(b.is_complete());
}

/// The noun round-trips, because `progress_line`'s callers pass a `&str` and the
/// board has to map it back without those 21 sites changing shape.
#[test]
fn every_kind_round_trips_through_its_noun() {
	for kind in [
		Kind::Network,
		Kind::Volume,
		Kind::Secret,
		Kind::Image,
		Kind::Container,
	] {
		assert_eq!(Kind::from_noun(kind.noun()), Some(kind));
	}
	assert_eq!(Kind::from_noun("Sandwich"), None);
}
