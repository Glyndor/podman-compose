//! Continuation of command dispatch -- the second half of the `dispatch` match.
//!
//! Split out of `dispatch.rs` to keep both files within the source line limit.
//! Reached via the catch-all arm in [`super::dispatch`]; the arms here move
//! their fields exactly as before.

use podup::{Engine, StatsOptions};

use crate::cli::*;

use super::profile_filtered;

/// Handle the commands not matched in [`super::dispatch`]. Behaviour-preserving
/// continuation of the same match (the arms are a verbatim move).
pub(super) async fn dispatch_rest(
	engine: &Engine,
	file: &podup::compose::types::ComposeFile,
	command: Commands,
	profile: &[String],
) -> podup::Result<()> {
	match command {
		Commands::Run {
			service,
			rm: _,
			no_rm,
			detach,
			env_overrides,
			name,
			service_ports,
			user: _,
			workdir: _,
			entrypoint: _,
			volume: _,
			publish: _,
			// Both act now: they reach the engine through `RunOverrides`, which
			// `run_overrides_for` builds from this same command.
			interactive: _,
			no_tty: _,
			no_deps: _,
			label: _,
			cmd,
		} => {
			engine
				.run(
					file,
					&service,
					podup::RunOptions {
						cmd,
						// Remove the one-off container after exit unless the user
						// asked to keep it with `--no-rm`.
						rm: !no_rm,
						detach,
						env_overrides,
						name_override: name,
						service_ports,
					},
				)
				.await?
		}
		Commands::Cp {
			src,
			dst,
			index,
			follow_link,
			archive,
		} => {
			engine
				.cp_with_options(
					file,
					&src,
					&dst,
					podup::CpOptions {
						index,
						follow_link,
						archive,
					},
				)
				.await?
		}
		Commands::Ps { .. } => unreachable!("handled before compose parsing"),
		Commands::Top { format, services } => {
			engine
				.top_with_options(file, &services, format == OutputFormat::Json)
				.await?
		}
		Commands::Events {
			format,
			since,
			until,
			filter,
			json,
		} => {
			// `--json` is the deprecated alias for `--format json` (and conflicts
			// with an explicit `--format`); either selects JSON-line output.
			let json = json || format == EventsFormat::Json;
			engine
				.stream_events_with_options(
					json,
					&podup::EventsOptions {
						since,
						until,
						filters: filter,
					},
				)
				.await?
		}
		Commands::Attach {
			service,
			index,
			no_stdin: _,
			sig_proxy: _,
			detach_keys: _,
		} => engine.attach_with_index(file, &service, index).await?,
		Commands::Wait { format, services } => {
			let file = &profile_filtered(file, profile, &services);
			engine
				.wait_services_with_options(file, &services, matches!(format, OutputFormat::Json))
				.await?
		}
		Commands::Commit {
			service,
			image,
			message,
			author,
			pause,
			change,
			index,
		} => {
			engine
				.commit_with_options(
					file,
					&service,
					&image,
					index,
					podup::CommitOptions {
						message,
						author,
						pause: Some(pause),
						changes: change,
					},
				)
				.await?
		}
		Commands::Export {
			service,
			output,
			index,
		} => engine.export(file, &service, output, index).await?,
		Commands::Stats {
			no_stream,
			all,
			no_trunc,
			format,
			services,
		} => {
			let opts = StatsOptions::new(no_stream, all, no_trunc, format == OutputFormat::Json);
			engine.stats_with_options(file, &services, opts).await?
		}
		Commands::Push {
			ignore_push_failures,
			tls_verify,
			quiet,
			services,
		} => {
			let file = &profile_filtered(file, profile, &services);
			engine
				.push_with_quiet(
					file,
					&services,
					podup::PushOptions {
						ignore_failures: ignore_push_failures,
						tls_verify,
					},
					quiet,
				)
				.await?
		}
		Commands::Port {
			service,
			private_port,
			proto,
			index,
		} => {
			engine
				.port_with_index(file, &service, &private_port, &proto, index)
				.await?
		}
		Commands::Volumes {
			quiet,
			format,
			services,
		} => {
			engine
				.list_volumes(
					file,
					&services,
					podup::VolumesOptions {
						quiet,
						json: format == OutputFormat::Json,
					},
				)
				.await?
		}
		Commands::Images {
			quiet,
			format,
			services,
		} => {
			let file = &profile_filtered(file, profile, &services);
			engine
				.images_with_services(
					file,
					&services,
					podup::ImagesOptions {
						quiet,
						json: format == OutputFormat::Json,
					},
				)
				.await?
		}
		Commands::Logs {
			follow,
			tail,
			since,
			until,
			timestamps,
			no_color,
			no_log_prefix,
			services,
		} => {
			engine
				.logs_with_display(
					file,
					&services,
					podup::LogsOptions {
						follow,
						tail,
						since,
						until,
						timestamps,
					},
					podup::LogsDisplay {
						no_color,
						no_log_prefix,
					},
				)
				.await?
		}
		Commands::Exec {
			env,
			user,
			workdir,
			privileged,
			detach,
			no_tty,
			index,
			service,
			cmd,
		} => {
			engine
				.exec_with_options(
					file,
					&service,
					cmd,
					podup::ExecOptions::new(env, user, workdir, privileged, detach, index, no_tty),
				)
				.await?
		}
		Commands::Pull {
			quiet: _,
			ignore_pull_failures,
			include_deps,
			policy: _,
			services,
		} => {
			let file = &profile_filtered(file, profile, &services);
			engine
				.pull_services_with_options(
					file,
					&services,
					podup::PullOptions {
						ignore_failures: ignore_pull_failures,
						include_deps,
					},
				)
				.await?
		}
		Commands::Restart {
			timeout: _,
			no_deps,
			services,
		} => {
			let file = &profile_filtered(file, profile, &services);
			engine
				.restart_with_options(file, &services, no_deps)
				.await?
		}
		Commands::Config { .. } => unreachable!("handled above"),
		Commands::Generate { .. } => unreachable!("handled above"),
		Commands::Autostart { .. } => unreachable!("handled above"),
		Commands::Watch => engine.watch(file).await?,
		Commands::Help { .. } => unreachable!("handled before compose parsing"),
		Commands::Version { .. } => unreachable!("handled before compose parsing"),
		Commands::Ls { .. } => unreachable!("handled before compose parsing"),
		#[cfg(feature = "update")]
		Commands::Update { .. } => unreachable!("handled before compose parsing"),
		#[cfg(feature = "completions")]
		Commands::Completions { .. } => unreachable!("handled before compose parsing"),
		_ => unreachable!("handled in dispatch"),
	}

	Ok(())
}
