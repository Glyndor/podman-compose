use super::{commit_path, refuse_tar_to_tty, split_image_ref};

#[test]
fn split_image_ref_defaults_tag() {
	assert_eq!(split_image_ref("myimage").unwrap(), ("myimage", "latest"));
	assert_eq!(split_image_ref("myimage:1.0").unwrap(), ("myimage", "1.0"));
}

#[test]
fn split_image_ref_keeps_registry_port() {
	// A ':' that is part of a registry host:port is not a tag.
	assert_eq!(
		split_image_ref("registry:5000/app").unwrap(),
		("registry:5000/app", "latest")
	);
	assert_eq!(
		split_image_ref("localhost:5000/app:v2").unwrap(),
		("localhost:5000/app", "v2")
	);
}

#[test]
fn split_image_ref_rejects_empty_and_empty_repo() {
	assert!(split_image_ref("").is_err());
	assert!(split_image_ref(":tag").is_err());
	assert!(split_image_ref("repo:").is_err());
}

#[test]
fn commit_path_includes_pause_flag() {
	// Pausing (docker default) yields a consistent snapshot.
	let paused = commit_path("proj_web_1", "repo", "latest", true);
	assert!(paused.contains("container=proj_web_1"));
	assert!(paused.contains("repo=repo"));
	assert!(paused.contains("tag=latest"));
	assert!(paused.contains("pause=true"));
	// Opting out keeps the container live during commit.
	let live = commit_path("proj_web_1", "repo", "latest", false);
	assert!(live.contains("pause=false"));
}

#[test]
fn refuses_only_when_no_file_and_tty() {
	assert!(refuse_tar_to_tty(true, true));
	assert!(!refuse_tar_to_tty(true, false));
	assert!(!refuse_tar_to_tty(false, true));
	assert!(!refuse_tar_to_tty(false, false));
}

#[test]
fn stdout_broken_pipe_is_a_clean_stop_only_on_stdout() {
	use super::is_stdout_broken_pipe;
	use std::io::{Error, ErrorKind};
	let epipe = || Error::from(ErrorKind::BrokenPipe);
	// A broken pipe while streaming to stdout (`| head`) is a clean stop.
	assert!(is_stdout_broken_pipe(true, &epipe()));
	// The same error against a `-o FILE` sink is a real failure to report.
	assert!(!is_stdout_broken_pipe(false, &epipe()));
	// Any other write error on stdout is still a real failure.
	assert!(!is_stdout_broken_pipe(
		true,
		&Error::from(ErrorKind::PermissionDenied)
	));
}

/// `io_to_err` is the sole producer of [`ComposeError::IoPath`] on the
/// `export` write paths: any write or flush error against a `-o FILE`
/// sink is wrapped with the destination path so the message tells the
/// operator which file failed, not the bare `io error:`.
#[test]
fn io_to_err_with_output_path_wraps_as_iopath() {
	use super::io_to_err;
	use crate::error::ComposeError;
	use std::io::{Error, ErrorKind};
	use std::path::PathBuf;

	let p = PathBuf::from("/tmp/out.x.tar");
	let e = Error::new(ErrorKind::PermissionDenied, "denied");
	let mapped = io_to_err(&Some(p.clone()), e);

	// The variant must be IoPath with the path attached — a bare
	// `ComposeError::Io(_)` would drop the destination the user just
	// asked us to write to, and is the regression this variant exists to
	// prevent.
	let ComposeError::IoPath { path, source } = mapped else {
		panic!("expected IoPath, got a different variant");
	};
	assert_eq!(path, p.display().to_string(), "path must be preserved");
	assert_eq!(
		source.kind(),
		ErrorKind::PermissionDenied,
		"underlying io error must round-trip"
	);
}

/// A stdout sink has no path to name, so `io_to_err` falls back to the
/// generic [`ComposeError::Io`] rather than fabricating an empty
/// `IoPath`. Pinning the asymmetry so a future refactor does not
/// "improve" it into always wrapping.
#[test]
fn io_to_err_without_output_path_falls_back_to_plain_io() {
	use super::io_to_err;
	use crate::error::ComposeError;
	use std::io::{Error, ErrorKind};

	let e = Error::new(ErrorKind::BrokenPipe, "epipe");
	let mapped = io_to_err(&None, e);
	assert!(
		matches!(mapped, ComposeError::Io(_)),
		"stdout sink must stay as ComposeError::Io, got {mapped:?}"
	);
}
