//! The `podup` subcommand enum, split from `mod.rs` to stay within the
//! source line limit as the CLI surface grows.

use std::path::PathBuf;

use clap::Subcommand;
#[cfg(feature = "completions")]
use clap_complete::Shell;

use super::parse::{parse_progress, parse_pull_policy, parse_scale_pair, parse_timeout};
use super::types::{
	AutostartCommands, ConfigFormat, EventsFormat, GenerateCommands, OutputFormat, RmiScope,
};

#[derive(Subcommand)]
pub(crate) enum Commands {
	/// Create and start all services.
	Up {
		/// Run containers in the background.
		#[arg(short, long)]
		detach: bool,
		/// Build images before starting containers.
		#[arg(long, conflicts_with = "no_build")]
		build: bool,
		/// Watch and sync/rebuild/restart per develop.watch rules.
		#[arg(short, long)]
		watch: bool,
		/// Remove containers for services not defined in the compose file.
		#[arg(long)]
		remove_orphans: bool,
		/// Do not recreate containers that are already running.
		#[arg(long, conflicts_with = "force_recreate")]
		no_recreate: bool,
		/// Recreate containers even if their configuration is unchanged.
		#[arg(long)]
		force_recreate: bool,
		/// Do not start linked services (depends_on) of the named services.
		#[arg(long)]
		no_deps: bool,
		/// Seconds to wait for a container to stop when recreating.
		#[arg(short = 't', long, allow_hyphen_values = true, value_parser = parse_timeout)]
		timeout: Option<i32>,
		/// Override the replica count for a service: SERVICE=N (repeatable).
		#[arg(long, value_parser = parse_scale_pair)]
		scale: Vec<(String, u32)>,
		/// Pull policy before starting: always, missing, never, newer, build.
		#[arg(long, value_parser = parse_pull_policy)]
		pull: Option<String>,
		/// Do not build images, even for services with a build section.
		#[arg(long)]
		no_build: bool,
		/// Suppress image-pull progress output.
		#[arg(long)]
		quiet_pull: bool,
		/// Wait until services are running/healthy before returning.
		#[arg(long)]
		wait: bool,
		/// Maximum seconds to wait with --wait before giving up.
		#[arg(long, requires = "wait")]
		wait_timeout: Option<u64>,
		/// Create containers but do not start them.
		#[arg(long)]
		no_start: bool,
		/// Prefix attached log lines with a timestamp (ignored with -d).
		#[arg(long)]
		timestamps: bool,
		/// Recreate anonymous volumes instead of keeping the previous ones.
		#[arg(short = 'V', long)]
		renew_anon_volumes: bool,
		/// Bring up only these services (and their depends_on); default: all.
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Stop and remove containers.
	Down {
		/// Also remove named volumes declared in the compose file.
		#[arg(short = 'v', long)]
		volumes: bool,
		/// Remove containers for services not defined in the compose file.
		#[arg(long)]
		remove_orphans: bool,
		/// Also remove service images: `all` or `local` (build-section services).
		#[arg(long, value_enum)]
		rmi: Option<RmiScope>,
		/// Seconds to wait for containers to stop before killing them.
		#[arg(short = 't', long, allow_hyphen_values = true, value_parser = parse_timeout)]
		timeout: Option<i32>,
	},
	/// Start existing stopped containers.
	Start {
		/// Wait until services are running/healthy before returning.
		#[arg(long)]
		wait: bool,
		/// Maximum seconds to wait with --wait before giving up.
		#[arg(long, requires = "wait")]
		wait_timeout: Option<u64>,
		/// Start only these services.
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Stop running containers without removing them.
	Stop {
		/// Seconds to wait for containers to stop before killing them.
		#[arg(short = 't', long, allow_hyphen_values = true, value_parser = parse_timeout)]
		timeout: Option<i32>,
		/// Stop only these services.
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Set the number of running containers for services.
	Scale {
		/// Target replica counts: SERVICE=N (one or more).
		#[arg(value_parser = parse_scale_pair, required = true)]
		pairs: Vec<(String, u32)>,
	},
	/// Create containers for services without starting them.
	Create {
		/// Build images before creating containers.
		#[arg(long)]
		build: bool,
		/// Recreate containers even if their configuration is unchanged.
		#[arg(long)]
		force_recreate: bool,
		/// Do not recreate containers that already exist.
		#[arg(long, conflicts_with = "force_recreate")]
		no_recreate: bool,
		/// Pull policy before creating: always, missing, never, newer, build.
		#[arg(long, value_parser = parse_pull_policy)]
		pull: Option<String>,
		/// Do not create linked services (depends_on) of the named services.
		#[arg(long)]
		no_deps: bool,
		/// Create only these services.
		services: Vec<String>,
	},
	/// Display a live stream of container resource usage.
	Stats {
		/// Disable streaming; print a single snapshot and exit.
		#[arg(long)]
		no_stream: bool,
		/// Include non-running containers as zeroed rows.
		#[arg(short, long)]
		all: bool,
		/// Do not truncate long container names in the table.
		#[arg(long)]
		no_trunc: bool,
		/// Output format.
		#[arg(long, value_enum, default_value_t = OutputFormat::Table)]
		format: OutputFormat,
		/// Show only these services.
		services: Vec<String>,
	},
	/// List podup compose projects on the host.
	Ls {
		/// Show all projects, including stopped ones.
		#[arg(short, long)]
		all: bool,
		/// Only display project names. Mutually exclusive with `--format`.
		#[arg(short, long, conflicts_with = "format")]
		quiet: bool,
		/// Filter projects by predicate: name=<NAME> or status=<running|exited>
		/// (repeatable).
		#[arg(long)]
		filter: Vec<String>,
		/// Output format.
		#[arg(long, value_enum, default_value_t = OutputFormat::Table)]
		format: OutputFormat,
	},
	/// Push service images to their registry.
	Push {
		/// Continue pushing the remaining services after a failure.
		#[arg(long)]
		ignore_push_failures: bool,
		/// Verify the registry TLS cert (false for insecure/HTTP; default on).
		#[arg(long)]
		tls_verify: Option<bool>,
		/// Suppress the push progress output.
		#[arg(short, long)]
		quiet: bool,
		/// Push only these services.
		services: Vec<String>,
	},
	/// Build or rebuild service images.
	Build {
		/// Do not use cache when building the image.
		#[arg(long)]
		no_cache: bool,
		/// Always attempt to pull a newer version of the base image.
		#[arg(long)]
		pull: bool,
		/// Set build-time variables (KEY=VAL); may be repeated.
		#[arg(long = "build-arg")]
		build_arg: Vec<String>,
		/// Set the build progress output style (auto, plain, tty); accepted for
		/// compatibility with other Compose tools. Validated but inert: podup renders build
		/// output one way.
		#[arg(long, value_parser = parse_progress)]
		progress: Option<String>,
		/// Push each built image to its registry after a successful build.
		#[arg(long)]
		push: bool,
		/// Suppress the build output.
		#[arg(short, long)]
		quiet: bool,
		/// Build only these services.
		services: Vec<String>,
	},
	/// Remove stopped service containers.
	#[command(alias = "remove")]
	Rm {
		/// Remove even running containers (stop first).
		#[arg(short, long)]
		force: bool,
		/// Also remove anonymous volumes attached to the containers.
		#[arg(short = 'v', long)]
		volumes: bool,
		/// Stop the containers (gracefully) before removing them.
		#[arg(short = 's', long)]
		stop: bool,
		/// Remove only these services.
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Send a signal to service containers.
	Kill {
		/// Signal to send (default: SIGKILL).
		#[arg(short, long, default_value = "SIGKILL")]
		signal: String,
		/// Also remove containers for services not in the compose file.
		#[arg(long)]
		remove_orphans: bool,
		/// Signal only these services.
		services: Vec<String>,
	},
	/// Pause running service containers.
	Pause {
		/// Pause only these services.
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Resume paused service containers.
	#[command(alias = "resume")]
	Unpause {
		/// Unpause only these services.
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Run a one-off command in a new service container.
	Run {
		/// Service to run the command against.
		service: String,
		/// Remove the container after it exits (the default).
		#[arg(long, overrides_with = "no_rm")]
		rm: bool,
		/// Keep the one-off container after it exits instead of removing it.
		#[arg(long, overrides_with = "rm")]
		no_rm: bool,
		/// Run container in the background.
		#[arg(short, long)]
		detach: bool,
		/// Set environment variables (KEY=VAL).
		#[arg(short, long = "env")]
		env_overrides: Vec<String>,
		/// Override the container name.
		#[arg(long)]
		name: Option<String>,
		/// Publish the service's declared ports (off by default).
		#[arg(short = 'P', long)]
		service_ports: bool,
		/// Run the command as this user (`name or UID[:GID]`).
		#[arg(short, long)]
		user: Option<String>,
		/// Working directory inside the container.
		#[arg(short, long)]
		workdir: Option<String>,
		/// Override the image entrypoint.
		#[arg(long)]
		entrypoint: Option<String>,
		/// Bind-mount an extra volume (HOST:CONTAINER[:OPTS] or NAME:CONTAINER); repeatable.
		#[arg(short = 'v', long = "volume")]
		volume: Vec<String>,
		/// Publish an extra port (HOST:CONTAINER[/PROTO]); repeatable.
		#[arg(short = 'p', long = "publish")]
		publish: Vec<String>,
		/// Keep the container's STDIN open (sets `stdin_open`). Whether a live
		/// terminal is attached is decided by stdin and stdout both being
		/// terminals, and by `-T` — not by this flag.
		#[arg(short, long)]
		interactive: bool,
		/// Disable pseudo-TTY allocation. `run` allocates one when stdin and
		/// stdout are both terminals; this opts out, so a script keeps plain
		/// streaming.
		// Both spellings are accepted on purpose: `docker compose run` spells the
		// long form `--no-TTY` while `docker compose exec` spells it `--no-tty`.
		// A script copied from either one has to work, so each command takes both.
		#[arg(short = 'T', long = "no-TTY", visible_alias = "no-tty")]
		no_tty: bool,
		/// Do not start linked services (depends_on) before running.
		#[arg(long)]
		no_deps: bool,
		/// Add a label to the one-off container (KEY=VAL); may be repeated.
		#[arg(short = 'l', long = "label")]
		label: Vec<String>,
		/// Command (and arguments) to run.
		#[arg(trailing_var_arg = true, allow_hyphen_values = true)]
		cmd: Vec<String>,
	},
	/// Copy files between a service container and the host (SERVICE:PATH for the
	/// container side, e.g. `web:/app/data ./local`).
	Cp {
		/// Source path. Use SERVICE:PATH for a container path.
		src: String,
		/// Destination path. Use SERVICE:PATH for a container path.
		dst: String,
		/// Target this replica index (1-based) of a scaled service.
		#[arg(long)]
		index: Option<u32>,
		/// Follow symlinks in the host source before copying into the container.
		#[arg(short = 'L', long)]
		follow_link: bool,
		/// Archive mode (accepted for compatibility; no effect under rootless Podman).
		#[arg(short = 'a', long)]
		archive: bool,
	},
	/// List containers.
	Ps {
		/// Show all containers, including stopped ones.
		#[arg(short, long)]
		all: bool,
		/// Only display container IDs. Mutually exclusive with `--format`.
		#[arg(short, long, conflicts_with = "format")]
		quiet: bool,
		/// Print the service names, one per line, instead of the container table.
		#[arg(long = "services")]
		services_only: bool,
		/// Show each container's on-disk size. Off by default: the server has
		/// to walk each container's writable layer to answer, which measured
		/// 21ms to 109ms over 59 containers.
		#[arg(short = 's', long)]
		size: bool,
		/// Filter containers by predicate: status=<running|exited> or
		/// name=<NAME> (repeatable).
		#[arg(long)]
		filter: Vec<String>,
		/// Filter by container status (running, exited, ...); repeatable.
		#[arg(long)]
		status: Vec<String>,
		/// Output format.
		#[arg(long, value_enum, default_value_t = OutputFormat::Table)]
		format: OutputFormat,
		/// Show only these services.
		services: Vec<String>,
	},
	/// Display the running processes of service containers.
	Top {
		/// Output format.
		#[arg(long, value_enum, default_value_t = OutputFormat::Table)]
		format: OutputFormat,
		/// Show only these services. Unlike the other service-list commands, `top`
		/// takes a plain positional (not `trailing_var_arg`) so `--format` parses
		/// in any position (`top web --format json` as well as `top --format json
		/// web`); service names are never hyphen-prefixed, so nothing is lost.
		services: Vec<String>,
	},
	/// Stream Podman events for this project's containers.
	Events {
		/// Output format: a `TYPE ACTION NAME` summary (table) or one JSON
		/// object per line (NDJSON, not a JSON array).
		#[arg(long, value_enum, default_value_t = EventsFormat::Table)]
		format: EventsFormat,
		/// Only stream events at or after this timestamp/relative time (e.g.
		/// -30m, 2026-01-01T00:00:00).
		///
		/// `allow_hyphen_values` because a relative time is written with a
		/// leading `-`; without it clap reads `-30m` as an unknown flag.
		#[arg(long, allow_hyphen_values = true)]
		since: Option<String>,
		/// Only stream events up to this timestamp/relative time. Only an
		/// already-elapsed window ends the feed: libpod keeps a stream open past
		/// a future time, so `--until -5m` bounds it and `--until 5m` does not.
		#[arg(long, allow_hyphen_values = true)]
		until: Option<String>,
		/// Filter events by predicate (KEY=VALUE, e.g. event=start); repeatable.
		#[arg(long)]
		filter: Vec<String>,
		/// Deprecated alias for `--format json`; kept for backward compatibility.
		/// Cannot be combined with an explicit `--format`.
		#[arg(long, hide = true, conflicts_with = "format")]
		json: bool,
	},
	/// Attach to a service container's output (stdout/stderr).
	Attach {
		/// Service whose container to attach to.
		service: String,
		/// Index of the container when the service has multiple replicas (1-based).
		#[arg(long)]
		index: Option<u32>,
		/// Do not attach STDIN (accepted for compatibility; podup
		/// attaches output only).
		#[arg(long)]
		no_stdin: bool,
		/// Proxy received signals to the process (accepted for compatibility).
		///
		/// Takes an optional value so both spellings parse. Other Compose tools
		/// declare it as a bare flag, and demanding a value turned a script
		/// copied from one of them into a usage error — inside the very set of
		/// flags whose purpose is that such a script runs unchanged.
		#[arg(
			long,
			default_value_t = false,
			action = clap::ArgAction::Set,
			num_args = 0..=1,
			default_missing_value = "true",
		)]
		sig_proxy: bool,
		/// Override the detach key sequence (accepted for compatibility).
		#[arg(long)]
		detach_keys: Option<String>,
	},
	/// Block until service containers stop, printing each exit code.
	Wait {
		/// Output format. `json` emits one NDJSON object per container as it
		/// exits, rather than after every container has.
		#[arg(long, value_enum, default_value_t = OutputFormat::Table)]
		format: OutputFormat,
		/// Wait on these services (default: all).
		services: Vec<String>,
	},
	/// Commit a service container to a new image.
	Commit {
		/// Service whose container to commit.
		service: String,
		/// Target image reference (repo[:tag]).
		image: String,
		/// Commit message for the new image.
		#[arg(short, long)]
		message: Option<String>,
		/// Author of the new image (e.g. "Name <email>").
		#[arg(short, long)]
		author: Option<String>,
		/// Apply a Containerfile instruction to the committed image, e.g.
		/// `CMD=/bin/sh` or `ENV=KEY=value` (repeatable).
		#[arg(short = 'c', long = "change")]
		change: Vec<String>,
		/// Replica index (1-based) of a scaled service.
		#[arg(long)]
		index: Option<u32>,
		/// Pause the container during commit for a consistent snapshot
		/// (default on; `--pause=false` to snapshot a live container).
		#[arg(
			short,
			long,
			default_value_t = true,
			action = clap::ArgAction::Set,
			num_args = 0..=1,
			default_missing_value = "true",
		)]
		pause: bool,
	},
	/// Export a service container's filesystem as a tar archive.
	Export {
		/// Service whose container to export.
		service: String,
		/// Write to this file instead of stdout.
		#[arg(short, long)]
		output: Option<PathBuf>,
		/// Replica index (1-based) of a scaled service.
		#[arg(long)]
		index: Option<u32>,
	},
	/// Print the public port for a port binding of a service container.
	Port {
		/// Service name.
		service: String,
		/// Private port, e.g. `80` or `80/tcp` (a `/proto` suffix overrides --protocol).
		private_port: String,
		/// Protocol (tcp or udp).
		#[arg(long, visible_alias = "protocol", default_value = "tcp")]
		proto: String,
		/// Index of the container when the service has multiple replicas (1-based).
		#[arg(long)]
		index: Option<u32>,
	},
	/// List the project's named volumes.
	#[command(alias = "volume")]
	Volumes {
		/// Only display volume names. Mutually exclusive with `--format`.
		#[arg(short, long, conflicts_with = "format")]
		quiet: bool,
		/// Show each volume's size and how much removing it would reclaim.
		/// Slow: the only endpoint carrying a volume's size accounts for the
		/// whole host, which measured 1.2s against 10ms for the plain list.
		#[arg(short = 's', long)]
		size: bool,
		/// Output format.
		#[arg(long, value_enum, default_value_t = OutputFormat::Table)]
		format: OutputFormat,
		/// Show only volumes mounted by these services.
		services: Vec<String>,
	},
	/// List images used by services.
	#[command(alias = "image")]
	Images {
		/// Only display image IDs. Mutually exclusive with `--format`.
		#[arg(short, long, conflicts_with = "format")]
		quiet: bool,
		/// Output format.
		#[arg(long, value_enum, default_value_t = OutputFormat::Table)]
		format: OutputFormat,
		/// Show images for these services only.
		services: Vec<String>,
	},
	/// View output from containers.
	#[command(alias = "log")]
	Logs {
		/// Follow log output.
		#[arg(short, long)]
		follow: bool,
		/// Number of lines to show from the end of the logs (default: all).
		#[arg(short = 'n', long)]
		tail: Option<String>,
		/// Show logs since a timestamp or relative time (e.g.
		/// 2024-01-01T00:00:00, -10m).
		#[arg(long, allow_hyphen_values = true)]
		since: Option<String>,
		/// Show logs before a timestamp or relative time (e.g. -5m).
		#[arg(long, allow_hyphen_values = true)]
		until: Option<String>,
		/// Prefix each line with an RFC3339 timestamp.
		#[arg(short = 't', long)]
		timestamps: bool,
		/// Produce monochrome output (no colour in the service-name prefix).
		#[arg(long)]
		no_color: bool,
		/// Do not print the service-name prefix on each line.
		#[arg(long)]
		no_log_prefix: bool,
		/// Show logs for these services (default: all).
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Execute a command in a running service container.
	Exec {
		/// Set environment variables (KEY=VAL); may be repeated.
		#[arg(short, long = "env")]
		env: Vec<String>,
		/// Run the command as this user (`name or UID[:GID]`).
		#[arg(short, long)]
		user: Option<String>,
		/// Working directory inside the container.
		#[arg(short, long)]
		workdir: Option<String>,
		/// Give extended privileges to the command.
		#[arg(long)]
		privileged: bool,
		/// Detach: run the command in the background.
		#[arg(short, long)]
		detach: bool,
		/// Disable pseudo-TTY allocation. `exec` allocates one when stdin and
		/// stdout are both terminals; this opts out, so a script keeps plain
		/// streaming.
		// `docker compose exec` spells the long form `--no-tty`, so that is the
		// primary here; `--no-TTY` stays accepted so the spelling `run` uses also
		// works on `exec`. See the note on `run`'s field.
		#[arg(short = 'T', long = "no-tty", visible_alias = "no-TTY")]
		no_tty: bool,
		/// Index of the container when the service has multiple replicas (1-based).
		#[arg(long)]
		index: Option<u32>,
		/// Service name.
		service: String,
		/// Command (and arguments) to execute.
		#[arg(trailing_var_arg = true, allow_hyphen_values = true)]
		cmd: Vec<String>,
	},
	/// Pull images for the named services, or all services if none are given.
	Pull {
		/// Suppress image-pull progress output.
		#[arg(short, long)]
		quiet: bool,
		/// Continue pulling the remaining services after a failure.
		#[arg(long)]
		ignore_pull_failures: bool,
		/// Also pull images for the named services' depends_on services.
		#[arg(long)]
		include_deps: bool,
		/// Pull policy: always, missing, never, newer, build (overrides per-service pull_policy).
		#[arg(long, value_parser = parse_pull_policy)]
		policy: Option<String>,
		/// Only pull images for these services.
		services: Vec<String>,
	},
	/// Restart services.
	Restart {
		/// Seconds to wait for containers to stop before killing them.
		#[arg(short = 't', long, allow_hyphen_values = true, value_parser = parse_timeout)]
		timeout: Option<i32>,
		/// Do not restart dependent services (depends_on with a restart condition).
		#[arg(long)]
		no_deps: bool,
		/// Restart only these services (default: all).
		#[arg(trailing_var_arg = true)]
		services: Vec<String>,
	},
	/// Print the resolved compose file (substitution/extends/include applied).
	#[command(alias = "convert")]
	Config {
		/// Output format.
		#[arg(long, value_enum, default_value_t = ConfigFormat::Yaml)]
		format: ConfigFormat,
		/// Print only the service names, one per line.
		#[arg(long)]
		services: bool,
		/// Print only the named volumes, one per line.
		#[arg(long)]
		volumes: bool,
		/// Print only the service image references, one per line.
		#[arg(long)]
		images: bool,
		/// Print only the profile names, one per line.
		#[arg(long)]
		profiles: bool,
		/// Print the config hash of all services, or of the given comma-separated
		/// services ("*" for all).
		#[arg(long)]
		hash: Option<String>,
		/// Only validate the configuration; print nothing.
		#[arg(short, long)]
		quiet: bool,
		/// Leave ${VAR} placeholders literal instead of interpolating them.
		#[arg(long)]
		no_interpolate: bool,
		/// Accepted for compatibility; podup output is already
		/// normalized.
		#[arg(long)]
		no_normalize: bool,
		/// Rewrite each service image to its registry digest (repo@sha256:...).
		#[arg(long)]
		resolve_image_digests: bool,
	},
	/// Generate declarative artifacts from the compose file.
	#[command(
		alias = "gen",
		subcommand_required = true,
		arg_required_else_help = true
	)]
	Generate {
		#[command(subcommand)]
		kind: GenerateCommands,
	},
	/// Manage a boot-time autostart unit for this compose project (rootless,
	/// user-scope `systemctl --user`).
	#[command(
		alias = "boot",
		subcommand_required = true,
		arg_required_else_help = true
	)]
	Autostart {
		#[command(subcommand)]
		kind: AutostartCommands,
	},
	/// Print help for podup, or for a specific command.
	Help {
		/// Command to show help for. Extra tokens, `-h`/`--help`, and a leading
		/// `--` are tolerated; only the first command name is used.
		#[arg(trailing_var_arg = true, allow_hyphen_values = true)]
		commands: Vec<String>,
	},
	/// Watch for file changes and sync/rebuild/restart as configured by develop.watch.
	Watch,
	/// Update podup to the latest signed release (Ed25519 signature + SHA-256
	/// verified against the embedded key; fails closed, leaving the binary
	/// untouched on any mismatch).
	#[cfg(feature = "update")]
	Update {
		/// Report whether a newer release exists without installing it.
		#[arg(long)]
		check: bool,
		/// Reinstall even if the latest release is not newer than this build.
		#[arg(long)]
		force: bool,
	},
	/// Print version information.
	Version {
		/// Print only the version number.
		#[arg(long)]
		short: bool,
		/// Output format: pretty or json.
		#[arg(long, value_parser = ["pretty", "json"], default_value = "pretty")]
		format: String,
	},
	/// Print a shell completion script to stdout for the named shell.
	#[cfg(feature = "completions")]
	Completions {
		/// Shell to generate completions for (bash, zsh, fish, powershell, elvish).
		shell: Shell,
	},
}
