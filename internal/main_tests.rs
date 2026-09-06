/// 130 is 128 + SIGINT, and it is what `docker compose up` returns for
/// SIGTERM too, measured against v5.1.3 rather than derived from the signal
/// number, which would have said 143. podup returned 0 for both, so a
/// cancelled CI job reported success.
#[test]
fn an_interrupt_maps_onto_the_shell_convention() {
	assert_eq!(interrupt_exit_code(), 130);
}
use super::*;
use clap::CommandFactory;

fn matches_for(args: &[&str]) -> clap::ArgMatches {
	Cli::command()
		.try_get_matches_from(args)
		.expect("args parse")
}

#[test]
fn update_flags_compose_globals_before_subcommand_are_rejected() {
	let m = matches_for(&[
		"podup",
		"--socket",
		"unix:///tmp/x.sock",
		"update",
		"--check",
	]);
	assert_eq!(first_misused_global(&m), Some("--socket"));
}

#[test]
fn update_flags_compose_globals_after_subcommand_are_rejected() {
	let m = matches_for(&["podup", "update", "--project-directory", "/tmp"]);
	assert_eq!(first_misused_global(&m), Some("--project-directory"));
}

#[test]
fn update_without_compose_globals_is_accepted() {
	let m = matches_for(&["podup", "update", "--check", "--force"]);
	assert_eq!(first_misused_global(&m), None);
}
