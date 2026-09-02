use super::{wait_header, wait_row, WAIT_NAME_WIDTH};

/// The exit code sits in one column, header included, whatever the container
/// name's length. `wait` prints as each container exits, so it cannot size
/// columns to a set it has not finished collecting.
#[test]
fn the_exit_column_is_in_one_place() {
	let code_col = |line: &str| {
		let plain: String = strip_ansi(line);
		plain.rfind(' ').map(|i| i + 1)
	};
	let header = wait_header();
	let short = wait_row("a", 0);
	let long = wait_row("project-service-name-12", 7);
	assert_eq!(code_col(&header), code_col(&short));
	assert_eq!(code_col(&header), code_col(&long));
	assert_eq!(code_col(&header), Some(WAIT_NAME_WIDTH + 1));
}

/// A name longer than the column truncates rather than shoving the code out
/// of alignment.
#[test]
fn an_over_long_name_truncates() {
	let row = strip_ansi(&wait_row(&"x".repeat(WAIT_NAME_WIDTH + 20), 0));
	assert_eq!(row.chars().count(), WAIT_NAME_WIDTH + 2);
}

/// A container name is not podup's own string, so it is escaped like every
/// other cell in the binary.
#[test]
fn a_container_name_cannot_drive_the_terminal() {
	// Colour is off in the test process, so any escape left in the output
	// came from the name rather than from the styling.
	let row = wait_row("evil\u{1b}[31m\u{7}name", 0);
	assert!(!row.contains('\u{1b}'), "{row:?}");
	assert!(!row.contains('\u{7}'), "{row:?}");
	assert!(row.contains("name"), "{row:?}");
}

fn strip_ansi(s: &str) -> String {
	let mut out = String::new();
	let mut chars = s.chars();
	while let Some(c) = chars.next() {
		if c == '\u{1b}' {
			for c in chars.by_ref() {
				if c == 'm' {
					break;
				}
			}
		} else {
			out.push(c);
		}
	}
	out
}
