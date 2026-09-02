use super::{ansi_from_argv, AnsiMode};

fn argv(args: &[&str]) -> impl Iterator<Item = String> + use<> {
	args.iter()
		.map(|s| s.to_string())
		.collect::<Vec<_>>()
		.into_iter()
}

/// clap renders help before the parsed `--ansi` is applied, so the flag has
/// to be read straight off argv or `podup --ansi never --help` comes out
/// coloured — which it did, while `NO_COLOR=1 podup --help` did not.
#[test]
fn ansi_is_found_before_the_subcommand() {
	assert_eq!(
		ansi_from_argv(argv(&["podup", "--ansi", "never", "--help"])),
		Some(AnsiMode::Never)
	);
	assert_eq!(
		ansi_from_argv(argv(&["podup", "--ansi=always", "up"])),
		Some(AnsiMode::Always)
	);
}

/// It is a global flag, so it is equally valid after the subcommand.
#[test]
fn ansi_is_found_after_the_subcommand() {
	assert_eq!(
		ansi_from_argv(argv(&["podup", "up", "--ansi", "never"])),
		Some(AnsiMode::Never)
	);
}

/// No flag means no opinion; the existing auto-detection decides.
#[test]
fn absent_ansi_yields_none() {
	assert_eq!(ansi_from_argv(argv(&["podup", "up", "-d"])), None);
	assert_eq!(ansi_from_argv(argv(&["podup", "--ansi"])), None);
	assert_eq!(ansi_from_argv(argv(&["podup", "--ansi", "sideways"])), None);
}

/// `--` ends podup's options. Everything after it is the container's command,
/// which `run`/`exec` forward verbatim with `allow_hyphen_values`, so a
/// passthrough argument reading `--ansi always` is not podup's flag.
#[test]
fn ansi_after_a_double_dash_is_not_ours() {
	assert_eq!(
		ansi_from_argv(argv(&["podup", "exec", "svc", "--", "--ansi", "always"])),
		None
	);
	assert_eq!(
		ansi_from_argv(argv(&[
			"podup",
			"exec",
			"svc",
			"--",
			"sh",
			"-c",
			"--ansi=never"
		])),
		None
	);
}

/// A flag before the `--` is still ours.
#[test]
fn ansi_before_a_double_dash_is_ours() {
	assert_eq!(
		ansi_from_argv(argv(&[
			"podup", "--ansi", "never", "exec", "svc", "--", "true"
		])),
		Some(AnsiMode::Never)
	);
}
