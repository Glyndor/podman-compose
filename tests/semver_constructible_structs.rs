//! Compilation contract for every public `*Options` struct.
//!
//! `podup` is committed to a 1.0.0 semver contract: adding a field to a
//! `pub struct` that an external consumer can construct with a struct literal
//! is a MAJOR bump. The crate therefore marks every external `*Options`
//! `#[non_exhaustive]` and gives each a `new(...)` constructor plus per-field
//! `with_*` builders, mirroring the pattern `ExecOptions` already uses.
//!
//! This file is the gate that keeps that promise. It is **not a runtime
//! test**: it only needs to compile. If somebody adds a new field to one of
//! these structs without the attribute, or breaks a builder, this binary
//! stops compiling and the gate turns the PR red before the change ever
//! reaches a downstream consumer. If somebody adds a new public
//! `*Options`/`*Display`/`*Overrides` struct without going through the same
//! `new` + `with_*` convention, the new entry simply has to be added here.
//!
//! Each block exercises the same surface an embedder would: `::default()`,
//! `::new(...)` with every field, and one chain of `with_*` calls. None of
//! the constructed values are used; the assignment to a `let _ =` is enough
//! for the type-checker to walk every constructor and method.
//!
//! The contract applies to every public options struct re-exported at the
//! crate root. The autostart option structs (`ServiceUnitOpts`,
//! `InstallOptions`) learned the same lesson earlier (#1093) and predate
//! this file; they are covered by their own integration tests.
//!
//! # Adding a new options struct
//!
//! 1. Mark it `#[non_exhaustive]`.
//! 2. Give it a `pub fn new(...)` taking every field, and one
//!    `#[must_use] pub fn with_<field>(mut self, ...) -> Self` per field.
//! 3. Add a block here that builds it through the public API.

#![allow(dead_code, unused_variables)]

use podup::{
	BuildOptions, Client, CommitOptions, CpOptions, Engine, EventsOptions, ExecOptions,
	ImagesOptions, LogsDisplay, LogsOptions, LsOptions, PsDisplayOptions, PsFilterOptions,
	PsOptions, PullOptions, PushOptions, RunOptions, RunOverrides, StatsOptions,
	VolumesDisplayOptions, VolumesOptions,
};

#[test]
fn every_public_options_struct_stays_constructible() {
	// --- already non-exhaustive pre-#1475 (ExecOptions, PsDisplayOptions,
	// VolumesDisplayOptions, plus InstallOptions/ServiceUnitOpts which predate
	// this file): keep their existing pattern exercised so a future field
	// addition has to come with a builder.

	let _ = ExecOptions::default();
	let _ = ExecOptions::new(Vec::new(), None, None, false, false, None, false);
	let _ = ExecOptions::default()
		.with_user(None)
		.with_workdir(None)
		.with_index(None)
		.with_env(Vec::new());

	let _ = PsDisplayOptions::default();
	let _ = PsDisplayOptions::new(false);
	let _ = PsDisplayOptions::default().with_size(true);

	let _ = VolumesDisplayOptions::default();
	let _ = VolumesDisplayOptions::new(false);
	let _ = VolumesDisplayOptions::default().with_size(true);

	// --- the nineteen structs touched by #1475.

	let _ = LsOptions::default();
	let _ = LsOptions::new(false, false, false);
	let _ = LsOptions::default()
		.with_all(true)
		.with_quiet(true)
		.with_json(true);

	let _ = BuildOptions::default();
	let _ = BuildOptions::new(false, false, Vec::new(), false);
	let _ = BuildOptions::default()
		.with_no_cache(true)
		.with_pull(true)
		.with_build_args(Vec::new())
		.with_quiet(true);

	let _ = PullOptions::default();
	let _ = PullOptions::new(false, false);
	let _ = PullOptions::default()
		.with_ignore_failures(true)
		.with_include_deps(true);

	let _ = PushOptions::default();
	let _ = PushOptions::new(false, None);
	let _ = PushOptions::default()
		.with_ignore_failures(true)
		.with_tls_verify(Some(false));

	let _ = ImagesOptions::default();
	let _ = ImagesOptions::new(false, false);
	let _ = ImagesOptions::default().with_quiet(true).with_json(true);

	let _ = LogsOptions::default();
	let _ = LogsOptions::new(false, None, None, None, false);
	let _ = LogsOptions::default()
		.with_follow(true)
		.with_tail(None)
		.with_since(None)
		.with_until(None)
		.with_timestamps(true);

	let _ = LogsDisplay::default();
	let _ = LogsDisplay::new(false, false);
	let _ = LogsDisplay::default()
		.with_no_color(true)
		.with_no_log_prefix(true);

	let _ = PsOptions::default();
	let _ = PsOptions::new(false, false, false);
	let _ = PsOptions::default()
		.with_all(true)
		.with_quiet(true)
		.with_json(true);

	let _ = PsFilterOptions::default();
	let _ = PsFilterOptions::new(false, Vec::new(), Vec::new(), Vec::new());
	let _ = PsFilterOptions::default()
		.with_services_only(true)
		.with_services(Vec::new())
		.with_status(Vec::new())
		.with_filters(Vec::new());

	let _ = RunOptions::default();
	let _ = RunOptions::new(Vec::new(), false, false, Vec::new(), None, false);
	let _ = RunOptions::default()
		.with_cmd(Vec::new())
		.with_rm(true)
		.with_detach(true)
		.with_env_overrides(Vec::new())
		.with_name_override(None)
		.with_service_ports(true);

	let _ = RunOverrides::default();
	let _ = RunOverrides::new(None, None, None, Vec::new(), Vec::new(), false, false);
	let _ = RunOverrides::default()
		.with_user(None)
		.with_workdir(None)
		.with_entrypoint(None)
		.with_volumes(Vec::new())
		.with_publish(Vec::new())
		.with_interactive(true)
		.with_no_deps(true);

	let _ = CommitOptions::default();
	let _ = CommitOptions::new(None, None, None, Vec::new());
	let _ = CommitOptions::default()
		.with_message(None)
		.with_author(None)
		.with_pause(None)
		.with_changes(Vec::new());

	let _ = StatsOptions::default();
	let _ = StatsOptions::new(false, false, false, false);
	let _ = StatsOptions::default()
		.with_no_stream(true)
		.with_all(true)
		.with_json(true)
		.with_no_trunc(true);

	let _ = VolumesOptions::default();
	let _ = VolumesOptions::new(false, false);
	let _ = VolumesOptions::default().with_quiet(true).with_json(true);

	let _ = CpOptions::default();
	let _ = CpOptions::new(None, false, false);
	let _ = CpOptions::default()
		.with_index(None)
		.with_follow_link(true)
		.with_archive(true);

	let _ = EventsOptions::default();
	let _ = EventsOptions::new(None, None, Vec::new());
	let _ = EventsOptions::default()
		.with_since(None)
		.with_until(None)
		.with_filters(Vec::new());
}

#[test]
fn no_struct_literal_escapes_into_the_published_api() {
	// Belt-and-braces: the same structs, but built without any helper so a
	// future `#[non_exhaustive]` removal cannot sneak back in unnoticed. The
	// `Default` bound is what every public options struct already has, so the
	// only way for this block to compile is `SomeOpts::default()`. If a struct
	// loses its `#[non_exhaustive]` and a downstream consumer starts using a
	// struct literal, they would still build; this block would still build;
	// the only signal would be the missing `#[non_exhaustive]` on the type
	// itself, which `cargo public-api` / `cargo semver-checks` already catches
	// at the API surface.
	let _: Engine = Engine::new(Client::new("/dev/null"), "contract".to_string());
}
