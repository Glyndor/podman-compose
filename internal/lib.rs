//! `podup` — docker-compose → Podman translator library.
//!
//! Provides parsing, variable substitution, topological ordering, and an
//! async engine that drives container lifecycle via Podman's native libpod
//! REST API over a Unix socket or Windows named pipe.

// `unsafe` is denied crate-wide; the few modules that need libc FFI opt back in
// locally with `#![allow(unsafe_code)]` and a soundness comment per block, so a
// new `unsafe` block elsewhere fails the build.
#![deny(unsafe_code)]

/// `podup autostart`: render and manage a rootless `systemctl --user` unit that
/// brings a compose stack up at boot (service mode).
pub mod autostart;
/// Compose-file parsing, `extends:`/`include:` resolution, and topological
/// service ordering.
pub mod compose;
pub(crate) mod dotenv;
pub(crate) mod engine;
/// `env_file:` loading: KEY=VALUE pairs from a service's declared files.
pub mod env_file;
pub(crate) mod error;
pub(crate) mod filesystem;
pub(crate) mod libpod;
/// Podman socket connection helpers.
pub mod podman;
/// Port-mapping parser for the docker-compose `ports:` format variants.
pub mod ports;
/// Quadlet export: translate a parsed compose file into Podman systemd units.
pub mod quadlet;
/// Memory and CPU value parsers shared by the engine and tests.
pub mod size;
/// Docker Compose `${VAR}`/`$VAR` substitution over raw YAML before parsing.
pub mod substitute;
pub(crate) mod timestamp;
/// Terminal colour/styling, honouring `--ansi`, `NO_COLOR`, and TTY detection.
pub mod ui;
pub(crate) mod units;
/// Secure self-update for the `podup` binary (signature-verified release fetch).
#[cfg(feature = "update")]
pub mod update;

/// Compose entry points: the parser variants, diagnostics collection, and
/// service-ordering helpers, re-exported at the crate root for callers.
pub use compose::{
	collect_diagnostics, parse_file, parse_file_with_env_files, parse_files_with_env_files,
	parse_files_with_env_files_interp, parse_str, parse_str_raw, resolve_levels, resolve_order,
	validate_config,
};
/// The lifecycle `Engine` and its per-command option/override types, plus the
/// project-name/listing helpers — the surface a CLI drives compose operations
/// through.
pub use engine::{
	is_safe_project_name, list_projects, list_projects_filtered, resolve_image_digests,
	retain_active_profiles, retain_active_profiles_with_targets, surface_host_modes,
	validate_stop_timeout, AttachOutcome, BuildOptions, CommitOptions, CpOptions, Engine,
	EventsOptions, ExecOptions, ImagesOptions, LogsDisplay, LogsOptions, LsOptions, ProjectLock,
	PsDisplayOptions, PsFilterOptions, PsOptions, PullOptions, PushOptions, RunOptions,
	RunOverrides, StatsOptions, VolumesDisplayOptions, VolumesOptions, DEFAULT_LOG_TAIL,
};
/// The crate's error type and `Result` alias, surfaced so callers handle one
/// error enum across parsing and engine calls.
pub use error::{ComposeError, Result};
/// The libpod `Client`, surfaced for callers that talk to Podman directly.
pub use libpod::Client;
/// The libpod error type carried inside [`ComposeError::Podman`], with the
/// predicates the engine's own retry paths use.
///
/// An embedding daemon has to tell a transport fault it should retry from a
/// rejection it should not, and the only alternative to these predicates is
/// matching on the message text — which breaks silently the day libpod rewords
/// one.
pub use libpod::PodmanError;
/// Log frames as libpod delivers them, and the stream parsers that produce
/// them, for callers routing container output somewhere other than this
/// process's stdout.
pub use libpod::{parse_json_lines, parse_multiplexed, parse_raw, LogOutput};

/// Internal parsers exposed only under `test-helpers` for fuzzing and tests.
///
/// These are not part of the public API (the feature is off by default, so the
/// published crate does not expose them); they let the fuzz harness reach the
/// crate-private dotenv parser, the libpod stream framer, and the
/// container→host tar extractor.
#[cfg(feature = "test-helpers")]
pub mod fuzz_api {
	pub use crate::dotenv::parse as dotenv_parse;
	pub use crate::engine::copy::archive::extract_tar_guarded;
	pub use crate::libpod::types::stream::{
		parse_frame, record_stream_bytes, take_json_line, MAX_STREAM_BUF,
	};
	pub use crate::libpod::PodmanError;
}
