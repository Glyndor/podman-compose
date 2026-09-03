//! Shared `ValueEnum` types for the CLI surface.

use clap::ValueEnum;

/// Output rendering for list commands (`ps`, `images`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
	/// Aligned columns for human reading.
	#[default]
	Table,
	/// Machine-readable JSON array.
	Json,
}

/// Output rendering for `events`. Distinct from [`OutputFormat`] because the
/// event stream is NDJSON (one object per line), not a JSON array, and the table
/// form is a plain summary line with no header — so the help text must not claim
/// "JSON array" / "aligned columns".
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum EventsFormat {
	/// A plain `TYPE ACTION NAME` summary, one event per line (no header/alignment).
	#[default]
	Table,
	/// One JSON object per line (NDJSON) — not a JSON array.
	Json,
}

/// When to colourise human-facing output (`--ansi`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum AnsiMode {
	/// Colour only when writing to a terminal (and `NO_COLOR` is unset).
	#[default]
	Auto,
	/// Always colour, even when piped or redirected.
	Always,
	/// Never colour.
	Never,
}

impl From<AnsiMode> for anstream::ColorChoice {
	fn from(m: AnsiMode) -> Self {
		match m {
			AnsiMode::Auto => Self::Auto,
			AnsiMode::Always => Self::Always,
			AnsiMode::Never => Self::Never,
		}
	}
}

/// Which images `down --rmi` removes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum RmiScope {
	/// All images used by the project's services.
	All,
	/// Only images built locally from a service `build:` section.
	Local,
}

/// Output rendering for `config`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum ConfigFormat {
	/// YAML (the compose-file format).
	#[default]
	Yaml,
	/// JSON.
	Json,
}

/// Which autostart backend `autostart install` sets up.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub(crate) enum AutostartMode {
	/// A single `Type=oneshot` `systemctl --user` service that runs `podup up -d`
	/// at boot and `podup stop` on shutdown. The default.
	#[default]
	Service,
	/// Per-service Podman Quadlet units: hand the stack to systemd as native
	/// `.container`/`.build`/`.volume`/`.network` units, so systemd owns boot,
	/// restart and dependency ordering directly.
	Quadlet,
	/// A single `Type=oneshot` `systemctl --user` service whose `ExecStart` is
	/// `podman start`, so the boot path resumes the container Podman already
	/// holds instead of reconciling it against the compose file. Single-service
	/// projects only.
	Start,
}

/// Subcommands of `autostart`.
#[derive(clap::Subcommand)]
pub(crate) enum AutostartCommands {
	/// Install (and, by default, enable + start) the autostart unit for this
	/// project. User-scope only: writes under `${XDG_CONFIG_HOME:-~/.config}`.
	Install {
		/// Autostart backend: `service` (one unit that runs `podup up`),
		/// `quadlet` (native systemd units, one per service), or `start`
		/// (resume the existing container; single-service projects only).
		#[arg(long, value_enum, default_value_t)]
		mode: AutostartMode,
		/// Install the unit but do not enable or start it immediately.
		#[arg(long)]
		no_start: bool,
		/// Print the unit and the actions that would run, but change nothing.
		#[arg(long)]
		dry_run: bool,
		/// In service mode, install a sibling `<unit>-update.service` /
		/// `<unit>-update.timer` pair that runs `podup up -d` on a schedule so
		/// the stack stays fresh without human action. Accepted only with
		/// `--mode service`; quadlet mode already drives auto-update through
		/// `podman-auto-update.timer`, and start mode has no compose front-end
		/// to run. `hourly`, `daily`, or `weekly`.
		#[arg(long, value_enum)]
		auto_update: Option<AutoUpdateInterval>,
	},
	/// Disable, stop, and remove this project's autostart unit.
	Uninstall {
		/// Also tear the stack down and remove its named volumes (`down -v`).
		#[arg(long)]
		purge: bool,
	},
	/// Report this project's autostart unit and session state.
	Status,
	/// Rebuild the image(s) of a quadlet-mode install and restart the container(s).
	///
	/// A Quadlet `.build` unit is `Type=oneshot`, so an image only rebuilds when its
	/// build service is restarted. This restarts `<project>-<service>-build.service`
	/// then the container service. Applies to `--mode quadlet` installs only.
	Rebuild {
		/// Rebuild only this service; omit to rebuild every built service.
		service: Option<String>,
	},
}

/// How often the autostart `--auto-update` timer fires. The `as_str` form is
/// the `OnCalendar=` value systemd expects, which is the same word.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum AutoUpdateInterval {
	/// `OnCalendar=hourly`.
	Hourly,
	/// `OnCalendar=daily`.
	Daily,
	/// `OnCalendar=weekly`.
	Weekly,
}

impl AutoUpdateInterval {
	/// The `OnCalendar=` value systemd expects.
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Hourly => "hourly",
			Self::Daily => "daily",
			Self::Weekly => "weekly",
		}
	}
}

/// Subcommands of `generate`.
#[derive(clap::Subcommand)]
pub(crate) enum GenerateCommands {
	/// Translate the compose file into Podman Quadlet unit files.
	///
	/// Emits one `.container` per service plus `.network` and `.volume` units.
	/// Without --output the units are printed to stdout; warnings about fields
	/// with no Quadlet mapping go to stderr.
	Quadlet {
		/// Directory to write the unit files into (e.g.
		/// ~/.config/containers/systemd). Prints to stdout when omitted.
		#[arg(short, long)]
		output: Option<std::path::PathBuf>,
	},
}
