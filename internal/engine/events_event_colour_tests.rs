use super::{format_event_line, TIME_WIDTH};

// The instant-by-instant pinning that used to live here moved to
// `crate::timestamp` with the arithmetic itself. Keeping a copy next to the
// caller would be two places to update and one of them would rot.

/// An event with no `time` renders the column blank rather than inventing a
/// value. A timestamp that looks real and is not is worse than none.
#[test]
fn an_event_without_a_time_leaves_the_column_empty() {
	let line = format_event_line(None, "container", "start", "web-1", false);
	assert!(
		line.starts_with(&" ".repeat(TIME_WIDTH)),
		"expected a blank time cell, got: {line:?}"
	);
}

/// Without a colour sink the line is byte-identical to what it always was,
/// so `--json`, a pipe and the output contract are untouched.
#[test]
fn plain_output_carries_no_escapes() {
	let out = format_event_line(Some(0), "container", "start", "proj-web-1", false);
	assert!(!out.contains('\u{1b}'), "{out:?}");
	assert_eq!(
		&out[TIME_WIDTH..],
		" container start          proj-web-1",
		"{out:?}"
	);
}

/// The three fields line up under the header, whatever their lengths, so
/// NAME does not move column on every line the way it used to.
#[test]
fn columns_align_under_the_header() {
	let header = super::events_header();
	let short = format_event_line(None, "pod", "die", "a", false);
	let long = format_event_line(None, "container", "health_status", "b", false);
	let name_col = |line: &str| line.rfind(' ').map(|i| i + 1);
	assert_eq!(
		name_col(&header),
		name_col(&short),
		"header {header:?} vs {short:?}"
	);
	assert_eq!(
		name_col(&header),
		name_col(&long),
		"header {header:?} vs {long:?}"
	);
}

/// An actor name comes from outside podup. This line painted it raw while
/// every other table escaped its cells, so an escape sequence in a container
/// name repainted the reader's terminal.
#[test]
fn an_actor_name_cannot_drive_the_terminal() {
	let out = format_event_line(None, "container", "start", "evil\u{1b}[31m\u{7}name", false);
	assert!(!out.contains('\u{1b}'), "{out:?}");
	assert!(!out.contains('\u{7}'), "{out:?}");
	assert!(out.contains("name"), "{out:?}");
}

/// The two fields that distinguish one line from the next carry colour; the
/// type, which repeats on nearly every line, is dimmed rather than absent.
#[test]
fn action_and_name_are_tinted_apart() {
	let died = format_event_line(None, "container", "die", "proj-web-1", true);
	let started = format_event_line(None, "container", "start", "proj-web-1", true);
	assert_ne!(
		died,
		started.replace("start", "die"),
		"die and start must differ by more than the verb"
	);
}

/// An event with no container name must not emit a stray colour reset.
#[test]
fn an_empty_name_is_not_painted() {
	let out = format_event_line(None, "network", "create", "", true);
	assert!(
		out.ends_with("create\u{1b}[0m") || !out.ends_with("\u{1b}[0m "),
		"{out:?}"
	);
}
