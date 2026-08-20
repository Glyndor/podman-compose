//! The public option structs for `images` and `logs`, and the per-field
//! builders that construct them.
//!
//! Held apart from `mod.rs` because they are published surface rather than
//! behaviour: every field is something an external caller sets, and every
//! `with_*` is part of the API the crate promises not to break.

/// Options for [`Engine::images_with_options`].
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`ImagesOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Default)]
#[non_exhaustive]
pub struct ImagesOptions {
	/// Print only image IDs, `-q/--quiet`.
	pub quiet: bool,
	/// Emit JSON instead of the table, `--format json`.
	pub json: bool,
}

impl ImagesOptions {
	/// Every `docker compose images` flag, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next flag to land is not a breaking change for anyone building one.
	pub fn new(quiet: bool, json: bool) -> Self {
		Self { quiet, json }
	}

	/// Print only image IDs, `-q/--quiet`. Builder-style.
	#[must_use]
	pub fn with_quiet(mut self, quiet: bool) -> Self {
		self.quiet = quiet;
		self
	}

	/// Emit JSON instead of the table, `--format json`. Builder-style.
	#[must_use]
	pub fn with_json(mut self, json: bool) -> Self {
		self.json = json;
		self
	}
}

/// Default for `podup logs` when the user does not pass `--tail`: show the last
/// 100 lines. `docker compose logs` defaults to "all"; podup's bounded default
/// keeps the inspection case ("what just happened?") from flooding the
/// terminal and stops CI scripts that capture `podup logs` from silently
/// missing errors that landed before the window. Pass `--tail all` to opt
/// back into the previous behaviour.
pub const DEFAULT_LOG_TAIL: &str = "100";

/// Options for [`Engine::logs_with_options`], mirroring `docker compose logs`.
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`LogsOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Default)]
#[non_exhaustive]
pub struct LogsOptions {
	/// Follow log output, `-f/--follow`.
	pub follow: bool,
	/// Number of lines to show from the end, `-n/--tail` (`None` = all).
	pub tail: Option<String>,
	/// Show logs since a timestamp/relative time, `--since`.
	pub since: Option<String>,
	/// Show logs until a timestamp/relative time, `--until`.
	pub until: Option<String>,
	/// Prefix each line with an RFC3339 timestamp, `-t/--timestamps`.
	pub timestamps: bool,
}

impl LogsOptions {
	/// Every `docker compose logs` flag, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next flag to land is not a breaking change for anyone building one.
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		follow: bool,
		tail: Option<String>,
		since: Option<String>,
		until: Option<String>,
		timestamps: bool,
	) -> Self {
		Self {
			follow,
			tail,
			since,
			until,
			timestamps,
		}
	}

	/// Follow log output, `-f/--follow`. Builder-style.
	#[must_use]
	pub fn with_follow(mut self, follow: bool) -> Self {
		self.follow = follow;
		self
	}

	/// Number of lines to show from the end, `-n/--tail` (`None` = all).
	/// Builder-style.
	#[must_use]
	pub fn with_tail(mut self, tail: Option<String>) -> Self {
		self.tail = tail;
		self
	}

	/// Show logs since a timestamp/relative time, `--since`. Builder-style.
	#[must_use]
	pub fn with_since(mut self, since: Option<String>) -> Self {
		self.since = since;
		self
	}

	/// Show logs until a timestamp/relative time, `--until`. Builder-style.
	#[must_use]
	pub fn with_until(mut self, until: Option<String>) -> Self {
		self.until = until;
		self
	}

	/// Prefix each line with an RFC3339 timestamp, `-t/--timestamps`.
	/// Builder-style.
	#[must_use]
	pub fn with_timestamps(mut self, timestamps: bool) -> Self {
		self.timestamps = timestamps;
		self
	}
}

/// Prefix-display options for [`Engine::logs_with_display`] (`docker compose
/// logs --no-color` / `--no-log-prefix`).
///
/// `#[non_exhaustive]` since 4.0.0, same rationale as [`LogsOptions`].
#[derive(Default)]
#[non_exhaustive]
pub struct LogsDisplay {
	/// Produce monochrome output (no colour in the prefix), `--no-color`.
	pub no_color: bool,
	/// Do not print the `{service} | ` prefix, `--no-log-prefix`.
	pub no_log_prefix: bool,
}

impl LogsDisplay {
	/// Both `docker compose logs` prefix flags, in CLI order. A constructor
	/// rather than a struct literal because the type is `#[non_exhaustive]`,
	/// so the next flag to land is not a breaking change for anyone building
	/// one.
	pub fn new(no_color: bool, no_log_prefix: bool) -> Self {
		Self {
			no_color,
			no_log_prefix,
		}
	}

	/// Produce monochrome output (no colour in the prefix), `--no-color`.
	/// Builder-style.
	#[must_use]
	pub fn with_no_color(mut self, no_color: bool) -> Self {
		self.no_color = no_color;
		self
	}

	/// Do not print the `{service} | ` prefix, `--no-log-prefix`.
	/// Builder-style.
	#[must_use]
	pub fn with_no_log_prefix(mut self, no_log_prefix: bool) -> Self {
		self.no_log_prefix = no_log_prefix;
		self
	}
}
