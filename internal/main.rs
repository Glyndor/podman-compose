//! `podup` — docker-compose to Podman translator CLI.

// The binary carries no `unsafe`; deny it so any future addition is caught.
#![deny(unsafe_code)]

use std::process;

#[cfg(feature = "completions")]
use clap::CommandFactory;

mod autostart_cmd;
mod cli;
mod dispatch;
mod exit_status;
mod generate;
mod resolve;
mod startup;

use cli::*;
use exit_status::{command_failure_exit_code, interrupt_exit_code, print_error};
use generate::write_quadlet;
use resolve::*;
use startup::{init_tracing, internal_error_notice, is_label_only, is_mutating, parse_cli};

fn main() {
	// Replace the default panic output (a raw Rust backtrace) with a `podup:`
	// internal-error notice that tells the user what to report and where, plus
	// the reminder to redact secrets first.
	std::panic::set_hook(Box::new(|info| {
		// A broken pipe (a downstream reader closed early, e.g. `podup ls | head`)
		// surfaces as a panic from the failing `println!`/`eprintln!` because Rust
		// ignores SIGPIPE. With `panic = "abort"` that would escalate to SIGABRT
		// (exit 134) and a misleading bug-report notice; instead exit quietly with
		// success like any well-behaved Unix tool.
		// Match the panic PAYLOAD, not `info.to_string()`: the latter also carries
		// the source location, widening a substring search to text the panic
		// itself never chose.
		let payload = info.payload();
		let message = payload
			.downcast_ref::<&str>()
			.copied()
			.or_else(|| payload.downcast_ref::<String>().map(String::as_str))
			.unwrap_or_default();
		if startup::is_broken_pipe_panic(message) {
			std::process::exit(0);
		}
		eprintln!("podup: internal error: {info}");
		eprintln!("{}", internal_error_notice());
	}));

	// Drive the runtime on a worker thread with a large stack. Clap's
	// command-building (debug builds especially) is stack-heavy and overflows
	// Windows' 1 MiB main-thread stack as the subcommand surface grows; an 8 MiB
	// matches Linux's default and leaves ample headroom.
	std::thread::Builder::new()
		.stack_size(8 * 1024 * 1024)
		.name("podup".into())
		.spawn(run_to_exit)
		.expect("spawn podup worker thread")
		.join()
		.expect("podup worker thread panicked");
}

/// Build the Tokio runtime and drive [`run`], mapping its result onto the
/// process exit status. Runs on the large-stack worker thread spawned by `main`.
fn run_to_exit() {
	let runtime = tokio::runtime::Builder::new_multi_thread()
		.enable_all()
		.build()
		.expect("build Tokio runtime");
	let outcome = runtime.block_on(run());
	// Shut the runtime down without waiting for its blocking pool. An
	// interactive exec/run always leaves one stdin read in flight there
	// (tokio's stdin reads on a blocking thread), and a plain drop waits for
	// it — so a session that ended with code 0 hung until the next keypress,
	// while any other code left through `process::exit` below and never
	// noticed (#1161). Everything user-visible is written and flushed by the
	// time `block_on` returns.
	runtime.shutdown_background();
	match outcome {
		Ok(()) => {}
		Err(podup::ComposeError::RunExited(code)) => process::exit(code as i32),
		// 128 + SIGINT, the shell convention, and what `docker compose up` returns
		// for SIGTERM as well as SIGINT. Nothing is printed: the operator pressed
		// Ctrl-C and knows it, and a compose file's own teardown output has
		// already gone to the terminal.
		Err(podup::ComposeError::Interrupted) => process::exit(interrupt_exit_code()),
		#[cfg(feature = "update")]
		Err(e @ podup::ComposeError::Update(_)) => {
			print_error(&e);
			process::exit(podup::update::exit_code(&e));
		}
		Err(e) => {
			print_error(&e);
			// A `run`/`exec` whose command cannot be launched arrives as a Podman
			// (OCI/crun) error; map it onto docker's conventional codes (127 not
			// found, 126 not executable) instead of the generic exit 1. Peel a
			// `DependencyNotReady` wrapper first so a failed readiness wait maps on
			// its real cause, exactly as an unwrapped one would.
			let code = match e.innermost() {
				podup::ComposeError::Podman(_) => command_failure_exit_code(&e.to_string()),
				_ => 1,
			};
			process::exit(code);
		}
	}
}

/// Resolve the Podman socket: an explicit `--socket` / `PODMAN_SOCKET` value
/// wins (clap already folds the env var into `cli.socket`); otherwise fall back
/// to `DOCKER_HOST` for Docker compatibility. A remote scheme on either is
/// rejected by [`podup::podman::connect`], so `DOCKER_HOST=tcp://…` fails closed
/// rather than being silently ignored.
fn resolve_socket(cli_socket: Option<&str>) -> Option<String> {
	cli_socket
		.map(str::to_string)
		.or_else(|| std::env::var("DOCKER_HOST").ok().filter(|s| !s.is_empty()))
}

/// Render help for `help [COMMAND]`, framed and colour-aware to match clap's own
/// `--help`. Help-flag tokens (`-h`/`--help`) and a leading `--` are tolerated;
/// the first remaining token selects the subcommand (an unknown one falls back
/// to the top-level help).
fn print_command_help(commands: &[String]) -> podup::Result<()> {
	use clap::CommandFactory;
	let mut cmd = Cli::command();
	let target = commands
		.iter()
		.find(|t| !matches!(t.as_str(), "-h" | "--help" | "--"));
	let rendered = match target.and_then(|name| cmd.find_subcommand_mut(name)) {
		Some(sub) => sub.render_long_help(),
		None => cmd.render_long_help(),
	};
	if podup::ui::stdout_colored() {
		print!("\n{}\n", rendered.ansi());
	} else {
		print!("\n{rendered}\n");
	}
	Ok(())
}

/// Whether a `down` invocation should tear the project down purely by its
/// `podup.project` label: the command is `down`, an explicit project name was
/// given (`-p` / `COMPOSE_PROJECT_NAME`), and no compose file resolves on disk.
/// Matches `docker compose -p NAME down` against a running project whose compose
/// file is absent. Pure so it is unit-testable without a Podman daemon.
fn down_by_label_path(command: &Commands, project: Option<&str>, compose_present: bool) -> bool {
	matches!(command, Commands::Down { .. }) && project.is_some() && !compose_present
}

/// Orchestrate one CLI invocation: parse args, then short-circuit the commands
/// that need neither a compose file nor (for some) Podman — `completions`,
/// `update`, `ls`, `ps`, and `config`. Otherwise resolve and parse the compose
/// file(s), settle the project name and base directory (validating the name at
/// the trust boundary), acquire the per-project lock, and dispatch the
/// remaining commands.
async fn run() -> podup::Result<()> {
	let cli = parse_cli();
	// Resolve the colour choice before any output (including tracing setup below)
	// so `--ansi`/`NO_COLOR`/TTY detection apply consistently everywhere.
	podup::ui::set_color_choice(cli.ansi.into());
	// Enable user-facing lifecycle progress for the CLI: per-container
	// Started/Stopped/Removed lines on stderr (and the `run -d` id on stdout),
	// matching docker compose. The library leaves this off so embedders stay
	// silent. Only the lifecycle commands emit it, so enabling it globally here
	// is inert for read-only/machine-output commands.
	podup::ui::set_progress(true);
	// `watch` is an interactive, long-running command; surface its per-action
	// progress (synced/rebuilt/restarted) by defaulting to INFO instead of the
	// quiet WARN floor. `RUST_LOG` always overrides.
	let log_floor = if matches!(cli.command, Commands::Watch) {
		"info"
	} else {
		"warn"
	};
	init_tracing(log_floor);

	// `help [COMMAND]` is served from the static CLI definition. Unlike clap's
	// built-in help subcommand it tolerates extra tokens, `-h`/`--help`, and a
	// leading `--`, and never errors with "unrecognized subcommand".
	if let Commands::Help { commands } = &cli.command {
		return print_command_help(commands);
	}

	// `version` mirrors `docker compose version` (scripts probe it); it needs
	// neither a compose file nor Podman.
	if let Commands::Version { short, ref format } = cli.command {
		use std::io::Write;
		let version = env!("CARGO_PKG_VERSION");
		let line = if short {
			version.to_string()
		} else if format == "json" {
			format!("{{\"version\":\"v{version}\"}}")
		} else {
			format!("podup version v{version}")
		};
		match writeln!(std::io::stdout(), "{line}") {
			Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => return Err(e.into()),
			_ => return Ok(()),
		}
	}

	// `completions` derives entirely from the static CLI definition; it neither
	// parses a compose file nor contacts Podman. Print to stdout for piping.
	#[cfg(feature = "completions")]
	if let Commands::Completions { shell } = cli.command {
		let mut cmd = Cli::command();
		let name = cmd.get_name().to_string();
		// Render into a buffer first: clap_complete panics if the writer errors,
		// which aborts (SIGABRT) when stdout is a closed pipe such as
		// `podup completions bash | head`. A `Vec` write never fails; then send
		// it to stdout and treat a broken pipe as a clean exit, like a normal
		// Unix tool.
		let mut buf = Vec::new();
		clap_complete::generate(shell, &mut cmd, name, &mut buf);
		match std::io::Write::write_all(&mut std::io::stdout(), &buf) {
			Ok(()) => return Ok(()),
			Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
			Err(e) => return Err(e.into()),
		}
	}

	// `update` operates on the binary itself, not a compose project, so it runs
	// before any compose file is parsed or Podman is contacted. The network and
	// filesystem work is blocking; keep it off the async path entirely.
	#[cfg(feature = "update")]
	if let Commands::Update { check, force } = cli.command {
		// The compose-only global value-flags (--socket/--profile/--env-file/
		// --project-directory) never reach the binary-only self-update, so accepting
		// them silently is a misleading no-op. Reject any that were passed on the
		// command line (env-sourced values are left alone — they are not a misuse).
		if let Some(flag) = misused_update_global() {
			use clap::CommandFactory;
			Cli::command()
				.error(
					clap::error::ErrorKind::ArgumentConflict,
					format!(
						"`{flag}` has no effect on `update`, which replaces the podup \
						 binary itself rather than acting on a compose project"
					),
				)
				.exit();
		}
		let opts = podup::update::UpdateOptions {
			check_only: check,
			force,
		};
		return tokio::task::spawn_blocking(move || podup::update::run(opts))
			.await
			.map_err(|e| podup::ComposeError::Update(format!("update task failed: {e}")))?;
	}

	// An explicit `--project-directory` must exist and be a directory before any
	// command relies on it (staging, lock files, quadlet output, base-dir
	// resolution); validate it once, at the trust boundary, for every command.
	validate_project_directory(cli.project_directory.as_deref())?;

	// `ls` discovers projects across the host by container label; it needs a
	// Podman connection but no compose file, so handle it before parsing one.
	if let Commands::Ls {
		all,
		quiet,
		filter,
		format,
	} = &cli.command
	{
		// `ls` is project-agnostic and short-circuits before compose parsing, so
		// validate the global `--env-file` paths it would otherwise silently
		// ignore (the other paths validate them while parsing).
		for ef in &cli.env_file {
			if !std::path::Path::new(ef).is_file() {
				return Err(podup::ComposeError::Unsupported(format!(
					"--env-file {ef} does not exist or is not a file"
				)));
			}
		}
		let client = podup::podman::connect_with_pool_size(
			resolve_socket(cli.socket.as_deref()).as_deref(),
			cli.connection_pool_size
				.unwrap_or(podup::Client::DEFAULT_POOL_SIZE),
		)?;
		return podup::list_projects_filtered(
			&client,
			podup::LsOptions {
				all: *all,
				quiet: *quiet,
				json: *format == OutputFormat::Json,
			},
			filter,
		)
		.await;
	}

	// `ps` filters running containers purely by the project label and ignores the
	// compose file entirely, so it must list an already-running project even when
	// that file is missing or unparseable (matching `docker compose ps`). Handle
	// it before the compose file is parsed: read the compose `name:` for project
	// precedence when the file parses, but fall back to the directory basename
	// rather than failing, unlike the commands that depend on the file's contents.
	if let Commands::Ps {
		all,
		quiet,
		services_only,
		size,
		filter,
		status,
		format,
		services,
	} = &cli.command
	{
		let compose_files = resolve_compose_files(&cli.file);
		let base_dir = resolve_base_dir(cli.project_directory.as_deref(), &compose_files[0]);
		// Parse the compose file when it is present and valid so `--services` and a
		// positional `SERVICE` filter can resolve service names and replicas;
		// otherwise fall back to an empty model (and the directory basename for the
		// project name) rather than failing.
		//
		// A file that is PRESENT but does not parse is a different case and is
		// propagated: the old `.ok()` swallowed it, so `podup ps` printed a bare
		// header and exited 0 on a broken compose file, which a health check or a
		// deploy gate reads as "nothing running, no error". `docker compose ps`
		// exits non-zero there, and so does every other podup command on the same
		// file.
		let parsed = match podup::parse_files_with_env_files(&compose_files, &cli.env_file) {
			Ok(f) => Some(f),
			Err(e) if compose_files.iter().any(|p| p.exists()) => return Err(e),
			Err(_) => None,
		};
		let compose_name = parsed.as_ref().and_then(|f| f.name.clone());
		let file = parsed.unwrap_or_default();
		let project = resolve_project_name(cli.project.clone(), compose_name.as_deref(), &base_dir);
		startup::validate_project_name(&project)?;
		// Identity colours key on the project-stripped label, so every command
		// tints the same container the same way.
		podup::ui::set_project(&project);
		// Register the service names so each gets its own identity colour. The
		// hash this replaces could not know how many services existed, so two
		// could share one. The `down`-by-label path has no compose file to read
		// service names from up front; it registers them itself, from the live
		// container listing it fetches immediately before tearing down.
		podup::ui::set_services(&file.services.keys().cloned().collect::<Vec<_>>());
		let client = podup::podman::connect_with_pool_size(
			resolve_socket(cli.socket.as_deref()).as_deref(),
			cli.connection_pool_size
				.unwrap_or(podup::Client::DEFAULT_POOL_SIZE),
		)?;
		let engine = podup::Engine::with_base_dir(client, project, base_dir);
		return engine
			.ps_filtered_with_display(
				&file,
				podup::PsOptions {
					all: *all,
					quiet: *quiet,
					json: *format == OutputFormat::Json,
				},
				podup::PsFilterOptions {
					services_only: *services_only,
					services: services.clone(),
					status: status.clone(),
					filters: filter.clone(),
				},
				podup::PsDisplayOptions::default().with_size(*size),
			)
			.await;
	}

	let compose_files = resolve_compose_files(&cli.file);

	// `down -p NAME` must tear a running project down purely from its
	// `podup.project` label when no compose file is present — matching `docker
	// compose -p NAME down`. The startup flow otherwise parses the compose file
	// before dispatch, so a label-only teardown by project name fails on the
	// missing file. Handle it here, before that parse, but only when an explicit
	// project name was given and no file resolves, so a stray `down` in an empty
	// directory still errors rather than guessing a project from the basename.
	if down_by_label_path(
		&cli.command,
		cli.project.as_deref(),
		compose_files.iter().any(|p| p.is_file()),
	) {
		if let Commands::Down {
			volumes,
			rmi,
			timeout,
			..
		} = &cli.command
		{
			let project = cli.project.clone().expect("checked by down_by_label_path");
			startup::validate_project_name(&project)?;
			// Identity colours key on the project-stripped label, so every command
			// tints the same container the same way.
			podup::ui::set_project(&project);
			// `--rmi` removes the images of the file's services, which cannot be
			// resolved without a file; warn rather than silently dropping it.
			if rmi.is_some() {
				tracing::warn!(
					"--rmi has no effect without a compose file; containers, networks and \
					 volumes are still removed by project label"
				);
			}
			let base_dir = resolve_base_dir(cli.project_directory.as_deref(), &compose_files[0]);
			let stop_timeout = podup::validate_stop_timeout(*timeout)?;
			let client = podup::podman::connect_with_pool_size(
				resolve_socket(cli.socket.as_deref()).as_deref(),
				cli.connection_pool_size
					.unwrap_or(podup::Client::DEFAULT_POOL_SIZE),
			)?;
			let engine = podup::Engine::with_base_dir(client, project, base_dir)
				.with_stop_timeout(stop_timeout);
			// `down` is mutating, so serialize it against concurrent runs as the
			// normal teardown path does.
			let _lock = engine.lock_project()?;
			return engine.down_by_label(*volumes).await;
		}
	}

	// `config --no-interpolate` must skip interpolation *entirely*: parsing with
	// interpolation enabled would evaluate a required-var `${VAR:?msg}` and fail
	// before we ever reached the no-interpolate branch. Detect it up front so the
	// file is parsed only once, with interpolation disabled.
	let no_interpolate = matches!(
		&cli.command,
		Commands::Config {
			no_interpolate: true,
			..
		}
	);
	// `events` and `ps` are scoped purely by the `podup.project` label and never
	// read service definitions, so — like `docker compose -p NAME events`/`ps` —
	// they must work against a running project even when no compose file is
	// present. Tolerate a missing file for these label-only commands by falling
	// back to an empty compose model; any other parse error (a malformed file
	// that *does* exist, a missing env file) still surfaces.
	let label_only = is_label_only(&cli.command);
	let file = if label_only && !compose_files.iter().any(|p| p.is_file()) {
		podup::compose::types::ComposeFile::default()
	} else {
		podup::parse_files_with_env_files_interp(&compose_files, &cli.env_file, !no_interpolate)?
	};

	if let Commands::Config {
		format,
		services,
		volumes,
		images,
		profiles,
		hash,
		quiet,
		resolve_image_digests,
		..
	} = &cli.command
	{
		// Validate the resolved project name in the config path too, at the same
		// trust boundary the mutating commands use, so `config -p 'bad name!'`
		// reports the same invalid-name error instead of succeeding. The config
		// path returns early below, so without this it would never be checked.
		let base_dir = resolve_base_dir(cli.project_directory.as_deref(), &compose_files[0]);
		let project = resolve_project_name(cli.project.clone(), file.name.as_deref(), &base_dir);
		startup::validate_project_name(&project)?;
		// Identity colours key on the project-stripped label, so every command
		// tints the same container the same way.
		podup::ui::set_project(&project);
		podup::ui::set_services(&file.services.keys().cloned().collect::<Vec<_>>());
		// `file` is already parsed with the correct interpolation setting above.
		let parsed = file;
		// `--resolve-image-digests` pins each image to its registry digest, which
		// needs a Podman connection to inspect images.
		let mut resolved = if *resolve_image_digests {
			let client = podup::podman::connect_with_pool_size(
				resolve_socket(cli.socket.as_deref()).as_deref(),
				cli.connection_pool_size
					.unwrap_or(podup::Client::DEFAULT_POOL_SIZE),
			)?;
			podup::resolve_image_digests(&client, &parsed).await?
		} else {
			parsed
		};
		// Honor active profiles so `config` prints the same services `up` starts.
		podup::retain_active_profiles(&mut resolved, &cli.profile);
		// Fold `env_file:` into `environment:` the way docker compose does, so a
		// service whose whole environment comes from a file does not render with no
		// environment at all (#1184). `config` is the command you reach for to ask
		// what will actually run; leaving the key unresolved pointed away from the
		// answer rather than merely omitting it.
		podup::env_file::materialize_env_files(&mut resolved, &base_dir)?;
		// Render the resolved project name (already settled above from -p /
		// COMPOSE_PROJECT_NAME, the top-level `name:`, then the directory
		// basename), like `docker compose config`, rather than echoing the file's
		// literal `name:`.
		return startup::render_config(
			&resolved,
			format,
			&startup::ConfigOutput {
				services: *services,
				volumes: *volumes,
				images: *images,
				profiles: *profiles,
				hash: hash.clone(),
				quiet: *quiet,
			},
			&project,
			&base_dir,
		);
	}

	let base_dir = resolve_base_dir(cli.project_directory.as_deref(), &compose_files[0]);
	let project = resolve_project_name(cli.project, file.name.as_deref(), &base_dir);

	// Validate the resolved project name at the trust boundary, before it reaches
	// any code path that builds a filesystem path from it (staging, lock files,
	// quadlet generation). Explicit `-p`/`COMPOSE_PROJECT_NAME` values and the
	// compose `name:` field are otherwise taken verbatim; rejecting an unsafe
	// name here fails closed regardless of which command runs next.
	startup::validate_project_name(&project)?;
	// Identity colours key on the project-stripped label, so ps, logs, stats and
	// the progress lines all tint the same container the same way. This is the
	// path every lifecycle command takes.
	podup::ui::set_project(&project);
	podup::ui::set_services(&file.services.keys().cloned().collect::<Vec<_>>());

	// `generate` produces declarative artifacts from the compose file alone; it
	// neither contacts Podman nor mutates project state.
	if let Commands::Generate {
		kind: GenerateCommands::Quadlet { output },
	} = &cli.command
	{
		// Honor active profiles so `generate quadlet` emits the same services
		// `up` would start, instead of also emitting units for inactive-profile
		// services (which would make `--profile` a no-op on this subcommand).
		let mut filtered = file.clone();
		podup::retain_active_profiles(&mut filtered, &cli.profile);
		// Absolute base dir so a `.build` unit's context resolves under the compose
		// file, not the unit directory the systemd generator would otherwise use.
		let base_dir = std::fs::canonicalize(&base_dir).unwrap_or(base_dir);
		return write_quadlet(
			&filtered,
			&project,
			&base_dir,
			output.as_deref(),
			cli.no_warn,
		);
	}

	// `autostart` manages a rootless `systemctl --user` unit that brings the stack
	// up at boot. Like `generate` it works from the compose file alone and never
	// contacts Podman — except `uninstall --purge`, which tears the stack's volumes
	// down and so connects only in that branch.
	if let Commands::Autostart { kind } = &cli.command {
		let env = autostart_cmd::AutostartEnv {
			profile: &cli.profile,
			env_files: &cli.env_file,
			socket: resolve_socket(cli.socket.as_deref()),
			connection_pool_size: cli.connection_pool_size,
		};
		return autostart_cmd::dispatch(&env, &compose_files, project, base_dir, &file, kind).await;
	}

	let client = podup::podman::connect_with_pool_size(
		resolve_socket(cli.socket.as_deref()).as_deref(),
		cli.connection_pool_size
			.unwrap_or(podup::Client::DEFAULT_POOL_SIZE),
	)?;
	// The `-t/--timeout` shutdown-grace override applies to every command that
	// stops containers (up recreate, down, stop, restart).
	let stop_timeout = match &cli.command {
		Commands::Up { timeout, .. }
		| Commands::Down { timeout, .. }
		| Commands::Stop { timeout, .. }
		| Commands::Restart { timeout, .. } => *timeout,
		_ => None,
	};
	// Reject a `-t/--timeout` below -1 here, at the trust boundary, with a clear
	// message instead of forwarding it to libpod as a raw `?t=<negative>` 400.
	let stop_timeout = podup::validate_stop_timeout(stop_timeout)?;
	// `--scale SERVICE=N` (on `up`) and the `scale` subcommand both feed the
	// engine's replica overrides so `resolve_replicas` reports the target count.
	let scale_overrides: std::collections::HashMap<String, u32> = match &cli.command {
		Commands::Up { scale, .. } => scale.iter().cloned().collect(),
		Commands::Scale { pairs } => pairs.iter().cloned().collect(),
		_ => std::collections::HashMap::new(),
	};
	// `up` image-acquisition overrides: `--pull`, `--no-build`, `--quiet-pull`.
	let (pull_override, no_build, quiet_pull) = match &cli.command {
		Commands::Up {
			pull,
			no_build,
			quiet_pull,
			..
		} => (pull.clone(), *no_build, *quiet_pull),
		Commands::Pull { quiet, policy, .. } => (policy.clone(), false, *quiet),
		Commands::Create { pull, .. } => (pull.clone(), false, false),
		_ => (None, false, false),
	};
	// `up -V/--renew-anon-volumes`: recreate anonymous volumes on container
	// recreation instead of leaving the old ones orphaned.
	let renew_anon_volumes = matches!(
		&cli.command,
		Commands::Up {
			renew_anon_volumes: true,
			..
		}
	);
	let engine = podup::Engine::with_base_dir(client, project, base_dir)
		.with_compose_files(compose_files.iter().map(|f| resolve::absolute(f)).collect())
		.with_stop_timeout(stop_timeout)
		.with_scale_overrides(scale_overrides)
		.with_up_overrides(pull_override, no_build, quiet_pull)
		.with_run_overrides(startup::run_overrides_for(&cli.command))
		.with_run_env_files(cli.env_file.clone())
		.with_run_labels(startup::run_labels_for(&cli.command))
		.with_run_no_tty(startup::run_no_tty_for(&cli.command))
		.with_renew_anon_volumes(renew_anon_volumes)
		.with_no_warn(cli.no_warn);

	// Serialize mutating lifecycle commands against concurrent `podup` runs on
	// the same project. Read-only / follow commands (ps, logs, top, port,
	// images, exec, pull, cp, config, watch) take no lock so they don't block
	// or get blocked. The guard is held until `run` returns.
	let _lock = if is_mutating(&cli.command) {
		Some(engine.lock_project()?)
	} else {
		None
	};

	dispatch::dispatch(&engine, &file, cli.command, &cli.profile).await
}

#[cfg(test)]
mod down_by_label_tests {
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
}

/// Compose-only global value-flags that `update` parses (they are declared
/// `global` on [`Cli`]) but cannot act on, since self-update rewrites the binary
/// itself rather than a compose project. Each entry is `(arg-id, user-facing
/// spelling)`.
#[cfg(feature = "update")]
const UPDATE_IRRELEVANT_GLOBALS: &[(&str, &str)] = &[
	("socket", "--socket"),
	("profile", "--profile"),
	("project_directory", "--project-directory"),
	("env_file", "--env-file"),
];

/// Return the first compose-only global flag that was supplied on the command
/// line for an `update` invocation, or `None` if none were. Re-parses the
/// already-validated argv to inspect value sources; env-sourced and default
/// values are deliberately ignored, so an exported `PODMAN_SOCKET` does not
/// break `podup update`.
#[cfg(feature = "update")]
fn misused_update_global() -> Option<&'static str> {
	use clap::CommandFactory;
	// Parsing already succeeded once in `parse_cli`, so this re-parse cannot fail.
	let matches = Cli::command().try_get_matches().ok()?;
	first_misused_global(&matches)
}

/// Core of [`misused_update_global`], split out so it can be tested against
/// matches built from a fixed argv. A global flag can surface on the root
/// matches or on the `update` subcommand matches depending on its position
/// relative to the subcommand, so both are checked.
#[cfg(feature = "update")]
fn first_misused_global(matches: &clap::ArgMatches) -> Option<&'static str> {
	use clap::parser::ValueSource;
	let update = matches.subcommand_matches("update");
	UPDATE_IRRELEVANT_GLOBALS.iter().find_map(|(id, flag)| {
		let from_cli = |m: &clap::ArgMatches| m.value_source(id) == Some(ValueSource::CommandLine);
		(from_cli(matches) || update.is_some_and(from_cli)).then_some(*flag)
	})
}

#[cfg(all(test, feature = "update"))]
mod tests {

	/// 130 is 128 + SIGINT, and it is what `docker compose up` returns for
	/// SIGTERM too — measured against v5.1.3 rather than derived from the signal
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
}
