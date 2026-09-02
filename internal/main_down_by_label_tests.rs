use super::down_by_label_path;
use crate::cli::Commands;

fn down() -> Commands {
	Commands::Down {
		volumes: false,
		remove_orphans: false,
		rmi: None,
		timeout: None,
	}
}

#[test]
fn down_with_project_and_no_file_takes_label_path() {
	// `down -p NAME` with no compose file present is the label-only teardown.
	assert!(down_by_label_path(&down(), Some("proj"), false));
}

#[test]
fn down_without_project_or_with_file_does_not() {
	// Without an explicit project name there is nothing to scope the teardown to,
	// and when a file is present the normal compose-parse path handles `down`.
	assert!(!down_by_label_path(&down(), None, false));
	assert!(!down_by_label_path(&down(), Some("proj"), true));
}

#[test]
fn other_commands_never_take_the_down_label_path() {
	// Only `down` is routed by label here; another command with `-p` and no file
	// must not be diverted.
	assert!(!down_by_label_path(&Commands::Watch, Some("proj"), false));
}
