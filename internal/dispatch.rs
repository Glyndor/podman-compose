//! Command dispatch: map a parsed `Commands` to engine calls.
//!
//! Split out of `main.rs` to keep that file within the source line limit as the
//! CLI surface grows. The match consumes the `Commands` value (arms move their
//! fields); `Config`/`Generate`/`Ls`/`Update`/`Completions` are handled earlier
//! in `main` and reached here only as `unreachable!` guards.

mod rest;

use podup::Engine;

use crate::cli::*;

/// Clone the compose file keeping only services in an active profile, plus any
/// named on the command line (which activates their profile). Used by the
/// per-service subcommands so they target exactly the set `up`/`create` would
/// bring up — never a service hidden behind an inactive profile.
fn profile_filtered(
	file: &podup::compose::types::ComposeFile,
	profile: &[String],
	targets: &[String],
) -> podup::compose::types::ComposeFile {
	let mut filtered = file.clone();
	podup::retain_active_profiles_with_targets(&mut filtered, profile, targets);
	filtered
}

/// Run the command against an already-built engine and parsed compose file.
pub(crate) async fn dispatch(
	engine: &Engine,
	file: &podup::compose::types::ComposeFile,
	command: Commands,
	profile: &[String],
) -> podup::Result<()> {
	match command {
		Commands::Up {
			detach,
			build,
			watch,
			remove_orphans,
			no_recreate,
			force_recreate,
			no_deps,
			timeout: _,
			scale: _,
			pull: _,
			no_build: _,
			quiet_pull: _,
			wait,
			wait_timeout,
			no_start,
			timestamps,
			renew_anon_volumes: _,
			abort_on_container_exit,
			exit_code_from,
			services,
		} => {
			if remove_orphans {
				engine.remove_orphans(file).await?;
			} else {
				engine.warn_orphans(file).await?;
			}
			// `--build` runs the build inside the `up` board (#1700): the image
			// rows are seeded at the top, built sequentially in `build_all`'s
			// order, then the network and container rows follow on the same
			// board. No separate `build_all` call, since that would draw its
			// own `[+] Running 0/N` board and close it before `up` opened its
			// own.
			// `--no-start` creates the containers but never starts them, so the
			// wait/watch/attach steps below do not apply.
			if no_start {
				engine
					.create_with_options(
						file,
						profile,
						&services,
						no_recreate,
						force_recreate,
						no_deps,
					)
					.await?;
				return Ok(());
			}
			engine
				.up_with_options(
					file,
					detach,
					profile,
					&services,
					no_recreate,
					force_recreate,
					no_deps,
					build,
				)
				.await?;
			if wait {
				// `--wait-timeout` bounds the health wait, like `start --wait`.
				let fut = engine.wait_services_healthy(file, &services);
				match wait_timeout {
					Some(secs) => tokio::time::timeout(std::time::Duration::from_secs(secs), fut)
						.await
						.map_err(|_| podup::ComposeError::WaitTimeout { secs })??,
					None => fut.await?,
				}
			}
			// `--wait` implies detach, as it does in docker compose: the flag
			// means "return once the services are up", so attaching afterwards
			// would block forever on a command whose whole point is to finish.
			// Measured against v5.1.3 on the same socket: `docker compose up
			// --wait` exits 0, and `podup up --wait` hung until killed — which is
			// the canonical health-gated CI idiom, so it hung the job.
			if watch {
				engine.watch(file).await?;
			} else if !detach && !wait {
				let summary = engine
					.attach_logs_with(
						file,
						&podup::AttachOptions::new()
							.with_timestamps(timestamps)
							.with_abort_on_container_exit(abort_on_container_exit)
							// `--exit-code-from` implies `--abort-on-container-exit`
							// in docker compose (measured against v5.1.3); the
							// underlying call applies that implication itself, so
							// all dispatch has to do is forward both flags verbatim.
							.with_exit_code_from(exit_code_from.clone()),
					)
					.await?;
				let outcome = summary.outcome;
				// Reported after the teardown, never instead of it: returning the
				// abort straight from `attach` would short-circuit the `?` above
				// and leave the project running, which is a worse bug than the
				// exit code. `attach` already stopped every container when it
				// returned `Aborted`, so the redundant `stop` here is skipped on
				// that path; the other three endings still need it.
				match outcome {
					podup::AttachOutcome::Aborted => {
						if let Some(code) = summary.exit_code {
							if code != 0 {
								return Err(podup::ComposeError::RunExited(code));
							}
						}
					}
					_ => {
						// A failed teardown must surface as a non-zero exit, not
						// be swallowed after the log streams end.
						engine.stop(file, &[]).await?;
					}
				}
				// Same ordering rule as the abort above: reported after the
				// teardown, never instead of it. docker compose exits 1 when an
				// attached stream dies with the container still running, and we
				// exited 0 — measured on 5.4.2 by restarting the libpod socket
				// underneath an attached `up`.
				match outcome {
					podup::AttachOutcome::Aborted => {}
					podup::AttachOutcome::Interrupted => {
						return Err(podup::ComposeError::Interrupted);
					}
					podup::AttachOutcome::StreamBroke => {
						return Err(podup::ComposeError::StreamTruncated(
							"log stream ended while the container was still \
							 running: output is incomplete"
								.to_string(),
						));
					}
					podup::AttachOutcome::StreamsEnded => {}
					_ => unreachable!("AttachOutcome is matched exhaustively above"),
				}
			}
		}
		Commands::Down {
			volumes,
			remove_orphans,
			rmi,
			timeout: _,
		} => {
			if remove_orphans {
				engine.remove_orphans(file).await?;
			}
			engine.down_with_options(file, volumes).await?;
			if let Some(scope) = rmi {
				engine
					.remove_service_images(file, scope == RmiScope::Local)
					.await?;
			}
		}
		Commands::Start {
			wait,
			wait_timeout,
			services,
		} => {
			let file = &profile_filtered(file, profile, &services);
			engine.start(file, &services).await?;
			if wait {
				match wait_timeout {
					// `--wait-timeout` both extends the per-service poll budget (so a
					// short healthcheck plan does not give up early) and caps the
					// whole wait, so exhaustion surfaces as a clear WaitTimeout rather
					// than a misleading per-container health-check timeout.
					Some(secs) => {
						let budget = std::time::Duration::from_secs(secs);
						let fut =
							engine.wait_services_healthy_within(file, &services, Some(budget));
						tokio::time::timeout(budget, fut)
							.await
							.map_err(|_| podup::ComposeError::WaitTimeout { secs })??;
					}
					None => engine.wait_services_healthy(file, &services).await?,
				}
			}
		}
		Commands::Stop {
			services,
			timeout: _,
		} => {
			let file = &profile_filtered(file, profile, &services);
			engine.stop(file, &services).await?
		}
		Commands::Scale { pairs } => engine.scale(file, &pairs).await?,
		Commands::Create {
			build,
			force_recreate,
			no_recreate,
			// `--pull` is applied via the engine's pull-policy override, set in
			// `main` (see `with_up_overrides`).
			pull: _,
			no_deps,
			services,
		} => {
			if build {
				engine.build_all(file, &services).await?;
			}
			engine
				.create_with_options(
					file,
					profile,
					&services,
					no_recreate,
					force_recreate,
					no_deps,
				)
				.await?
		}
		Commands::Build {
			no_cache,
			pull,
			build_arg,
			// `--progress` selects a progress renderer in docker compose; accepted
			// for compatibility but has no effect on podup's tracing output.
			progress: _,
			push,
			quiet,
			services,
		} => {
			let file = &profile_filtered(file, profile, &services);
			engine
				.build_all_with_options(
					file,
					&services,
					&podup::BuildOptions::new(no_cache, pull, build_arg, quiet),
				)
				.await?;
			// `--push` pushes each freshly built image to its registry.
			if push {
				engine
					.push_with_quiet(file, &services, podup::PushOptions::default(), quiet)
					.await?;
			}
		}
		Commands::Rm {
			force,
			volumes,
			stop,
			services,
		} => {
			// `-s/--stop` gracefully stops the targets first so they can be
			// removed without `--force`.
			// A paused container cannot be stopped (Podman rejects it with a raw
			// state error), so resume it first — matching `docker compose rm
			// -s`. `unpause` is idempotent, so this is a no-op when not paused.
			if stop {
				engine.unpause_paused(file, &services).await?;
				engine.stop(file, &services).await?;
			}
			engine
				.rm_with_options(file, &services, force, volumes)
				.await?
		}
		Commands::Kill {
			signal,
			remove_orphans,
			services,
		} => {
			let filtered = profile_filtered(file, profile, &services);
			engine.kill(&filtered, &services, &signal).await?;
			if remove_orphans {
				engine.remove_orphans(file).await?;
			}
		}
		Commands::Pause { services } => {
			let file = &profile_filtered(file, profile, &services);
			engine.pause(file, &services).await?
		}
		Commands::Unpause { services } => {
			let file = &profile_filtered(file, profile, &services);
			engine.unpause(file, &services).await?
		}
		other => rest::dispatch_rest(engine, file, other, profile).await?,
	}

	Ok(())
}
