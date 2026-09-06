//! One-off `run` command: start a throwaway container for a service, stream its
//! output, and remove it when done.

use std::collections::HashMap;
use std::io::Write;

use futures_util::StreamExt;

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};

use super::RunOptions;
use crate::engine::Engine;
use crate::libpod::API_PREFIX;

impl Engine {
	/// Run a one-off command in a new container for a service.
	///
	/// The container is started, its output streamed, and it is removed when done
	/// (unless `opts.rm` is false).
	///
	/// # Errors
	///
	/// A command that ran and exited non-zero surfaces as
	/// [`ComposeError::RunExited`](crate::ComposeError::RunExited), carrying its
	/// code.
	///
	/// A stream that dies while the container is still running is a failed read,
	/// not a finished command: it returns the transport error and never reaches
	/// the exit-code wait, so there is no code to carry. A stream that ends after
	/// the container has stopped is treated as finished, whether or not its body
	/// was terminated properly, and the exit code is reported as usual.
	pub async fn run(
		&self,
		file: &ComposeFile,
		service_name: &str,
		opts: RunOptions,
	) -> Result<()> {
		let RunOptions {
			cmd,
			rm,
			detach,
			env_overrides,
			name_override,
			service_ports,
		} = opts;
		// CLI-only run flags arrive via the engine builder (see `RunOverrides`),
		// keeping the public `RunOptions` API frozen at 1.0.
		let super::RunOverrides {
			user,
			workdir,
			entrypoint,
			volumes,
			publish,
			interactive,
			no_deps,
		} = self.run_overrides.clone();
		let no_tty = self.run_no_tty;
		// Same rule as `exec`: a TTY on both ends by default, `-T` to opt out,
		// and only when stdin is actually a terminal, so a script or a pipeline
		// keeps the streaming path it has always had, unchanged.
		let want_tty = crate::engine::wants_interactive_run(no_tty, detach);
		// `--env-file` and `-l/--label` are carried on the engine (not
		// `RunOverrides`) to keep the public struct frozen.
		let env_files = self.run_env_files.clone();
		let labels = self.run_labels.clone();
		let service = file
			.services
			.get(service_name)
			.ok_or_else(|| ComposeError::ServiceNotFound(service_name.into()))?;

		// Reject any bad volume/network/container name before creating anything
		// (the run path pre-creates the project networks below).
		self.validate_object_names(file)?;

		// Compose `run` brings up the service's `depends_on` services first (and
		// waits on their conditions), unless `--no-deps` is given. The service
		// itself is excluded; only its transitive dependencies are started.
		if !no_deps {
			let deps: Vec<String> =
				super::targets::expand_targets(file, &[service_name.to_string()], false)
					.map(|set| set.into_iter().filter(|n| n != service_name).collect())
					.unwrap_or_default();
			if !deps.is_empty() {
				self.up_with_options(file, true, &[], &deps, false, false, false, false)
					.await?;
			}
		}

		// A user-supplied `--name` is taken verbatim (no project prefix), so it can
		// collide with an arbitrary pre-existing container. docker compose errors on
		// such a conflict; podup must NOT force-remove the unrelated container (the
		// idempotent recreate in `create_and_start` would otherwise delete it). The
		// auto-generated default name carries the PID and is unique, so it never
		// needs this guard.
		let user_named = name_override.is_some();
		let run_name = name_override.unwrap_or_else(|| {
			format!("{}-{service_name}-run-{}", self.project, std::process::id())
		});

		let mut run_service = service.clone();
		if !cmd.is_empty() {
			run_service.command = Some(crate::compose::types::Command::Exec(cmd));
		}
		// `--entrypoint` overrides the image/service entrypoint with a single
		// executable token (compose/`docker run` semantics); any `cmd` becomes
		// its arguments.
		if let Some(ep) = entrypoint {
			run_service.entrypoint = Some(crate::compose::types::Command::Exec(vec![ep]));
		}
		if let Some(u) = user {
			run_service.user = Some(u);
		}
		if let Some(w) = workdir {
			run_service.working_dir = Some(w);
		}
		// `-i/--interactive` keeps STDIN open on the spec. With a terminal on both
		// ends the container also gets a live stdin attached below; without one
		// this is the same flag it always was.
		if interactive || want_tty {
			run_service.stdin_open = Some(true);
		}
		// Ad-hoc `-v/--volume` mounts append to the service's own mounts in
		// compose short form, parsed downstream like compose file entries.
		for v in volumes {
			run_service
				.volumes
				.push(crate::compose::types::VolumeMount::Short(v));
		}
		// `-l/--label` adds ad-hoc labels to the one-off container, merged over the
		// service's own labels in compose `KEY=VALUE` list form.
		if !labels.is_empty() {
			let mut list: Vec<String> = run_service
				.labels
				.to_map()
				.into_iter()
				.map(|(k, v)| if v.is_empty() { k } else { format!("{k}={v}") })
				.collect();
			list.extend(labels);
			run_service.labels = crate::compose::types::Labels::List(list);
		}
		// Layer the run container's environment by precedence, matching
		// `docker compose run --env-file`: global `--env-file` contents are the
		// lowest layer, the service's own `environment:` overrides them, and `-e`
		// overrides win over both.
		let env_file_vars = if env_files.is_empty() {
			HashMap::new()
		} else {
			crate::env_file::load_env_files(&env_files, &self.base_dir)?
		};
		if !env_file_vars.is_empty() || !env_overrides.is_empty() {
			run_service.environment = crate::compose::types::EnvVars::List(merge_run_environment(
				env_file_vars,
				run_service.environment.to_map(),
				env_overrides,
			));
		}
		run_service.restart = None;
		// Compose `run` does not publish the service's ports unless
		// `--service-ports` is given; otherwise a one-off run would collide
		// with the long-running service's host-port bindings.
		if !service_ports {
			run_service.ports.clear();
		}
		// Explicit `-p/--publish` ports are always bound, even without
		// `--service-ports`, matching `docker compose run -p`.
		for p in publish {
			run_service
				.ports
				.push(crate::compose::types::PortMapping::Short(p));
		}
		// Non-TTY forces Podman's multiplexed log framing, which is what the
		// streaming path below decodes: TTY mode sends raw bytes with no 8-byte
		// header and would garble that reader. The interactive path does not use
		// that reader at all: it attaches to the raw stream, which is exactly the
		// framing a TTY produces. So the two are consistent, not contradictory.
		run_service.tty = want_tty.then_some(true);

		// Seed the board with the rows `run` reports before the container's own
		// output takes over: the project networks, and the image when it still
		// has to be acquired. Asked here rather than derived from the compose
		// file alone, because whether the image is present is the one part of
		// that list the file cannot answer.
		let image_missing = match run_service.image.as_deref() {
			Some(image) => !self.image_present(image).await,
			None => false,
		};
		crate::ui::progress::begin(self.run_resources(file, &run_service, image_missing));
		let prepared = async {
			// Ensure the project networks exist (compose `run` brings them up like
			// `up` does); the service may reference the synthesized `default`
			// network, which is created here as `{project}_default`.
			self.create_networks(file).await?;
			// Inline secrets/configs are created up front (no longer in the
			// per-container build path), so materialise them here too before the run
			// container is created.
			self.create_project_secrets(file).await?;
			// `x-podman-pod`: ensure the pod exists with the current hash. A run
			// container joins the same pod as the project's `up` would.
			if file.podman_pod().map_err(ComposeError::Unsupported)? {
				let pod_ports: Vec<Vec<crate::ports::ParsedPort>> = file
					.services
					.values()
					.map(|s| crate::ports::parse_ports(&s.ports))
					.collect::<crate::error::Result<Vec<_>>>()?;
				// `run` creates one fresh container, so a recreated pod changes nothing here.
				self.ensure_pod(file, &pod_ports).await?;
			}

			// Refuse to clobber a pre-existing container of the same name (data-loss
			// footgun): `create_and_start` would force-remove it. Only the verbatim
			// user-supplied name can collide with something we don't own.
			if user_named && self.container_exists(&run_name).await? {
				return Err(ComposeError::Unsupported(format!(
					"the container name \"{run_name}\" is already in use; remove the existing \
					 container or choose a different --name"
				)));
			}
			Ok(())
		}
		.await;
		// Close the board before the container's own output starts: it is the
		// container that owns the terminal from here on, and a region left open
		// would repaint over its output. `end` is idempotent, so the error path
		// below leaves no hidden cursor either.
		crate::ui::progress::end();
		prepared?;

		let rm_path = format!(
			"{API_PREFIX}/containers/{}?force=true",
			crate::libpod::urlencoded(&run_name),
		);

		// On a start failure (bad --workdir/--user/--entrypoint), the container is
		// created but never starts; with --rm, remove it here so repeated failures
		// don't accumulate orphaned 'Created' containers.
		// Interactive runs create first and start later, with the attach in
		// between: a container started before anything is listening loses
		// whatever it printed in that gap, and for `run`, a one-shot command,
		// that gap is often the entire output.
		if let Err(e) = self
			.create_and_start(&run_name, service_name, &run_service, file, !want_tty)
			.await
		{
			if rm {
				let _ = self.client.delete_ok(&rm_path).await;
			}
			return Err(e);
		}

		if detach {
			// Echo the started container's name to stdout (gated by progress
			// output), so scripts capturing stdout get an id like
			// `docker compose run -d`.
			crate::ui::result_line(&run_name).map_err(ComposeError::Io)?;
			return Ok(());
		}

		// The interactive path takes over here: it needs the connection kept open
		// in both directions, which the request/response client cannot give it.
		// Same shape `exec` uses.
		if want_tty {
			return self.finish_interactive_run(&run_name, rm, &rm_path).await;
		}

		// Stream logs and wait for the exit code. Any failure on this path also
		// triggers the --rm cleanup below, so a failed stream/wait never leaks the
		// running container either. The wait result is captured before cleanup so a
		// failed wait surfaces as an error rather than masked as a successful run.
		let outcome: Result<i64> = async {
			let logs_path = format!(
				"{API_PREFIX}/containers/{}/logs?follow=true&stdout=true&stderr=true",
				crate::libpod::urlencoded(&run_name),
			);
			let logs_resp = self
				.client
				.get_stream(&logs_path)
				.await
				.map_err(ComposeError::Podman)?;
			let mut log_stream = crate::libpod::parse_multiplexed(logs_resp.into_body());

			// Lock stdout once for the whole stream instead of re-acquiring the lock
			// (and issuing a syscall) per frame; stdout is ours exclusively on this
			// path. stderr is locked per frame because the tracing subscriber also
			// writes there: holding its lock across the await loop would starve
			// concurrent log emissions. Flush after each frame so `run` streams
			// promptly.
			// Held across the loop but dropped before the recheck below, which
			// awaits: keeping stdout locked across that await would block any
			// concurrent writer for the length of an API round trip.
			let broke = {
				let mut out = std::io::stdout().lock();
				let mut broke = None;
				while let Some(msg) = log_stream.next().await {
					match msg {
						Ok(crate::libpod::LogOutput::StdOut { message }) => {
							let _ = write_frame(&mut out, &message);
							let _ = out.flush();
						}
						Ok(crate::libpod::LogOutput::StdErr { message }) => {
							let mut err = std::io::stderr().lock();
							let _ = write_frame(&mut err, &message);
							let _ = err.flush();
						}
						// This arm used to abort the run. A lost chunked terminator
						// is indistinguishable at the transport layer from a real
						// mid-stream break (#1104), so aborting here fails a run
						// whose command actually succeeded, on any version that
						// drops it. Deciding out of band instead: the container's
						// own state answers what the transport cannot.
						Err(e) => {
							broke = Some(e);
							break;
						}
					}
				}
				broke
			};
			if let Some(e) = broke {
				// Still running means the output really was truncated and the
				// stream failed. Stopped means the command finished and only the
				// terminator went missing, so fall through to `wait`, which
				// reports the exit code the run actually produced.
				if super::super::query::stream_broke_mid_output(
					self.container_still_running(&run_name).await,
				) {
					return Err(ComposeError::Podman(e));
				}
				tracing::debug!("run {run_name}: log stream ended as the container stopped: {e}");
			}

			let wait_path = format!(
				"{API_PREFIX}/containers/{}/wait?condition=stopped",
				crate::libpod::urlencoded(&run_name),
			);
			self.client
				.post_empty_json_unbounded::<i64>(&wait_path)
				.await
				.map_err(ComposeError::Podman)
		}
		.await;

		if rm {
			if let Err(e) = self.client.delete_ok(&rm_path).await {
				tracing::debug!("run cleanup delete {run_name}: {e}");
			}
		}

		let exit_code = outcome?;
		if exit_code != 0 {
			return Err(crate::error::ComposeError::RunExited(exit_code));
		}

		Ok(())
	}
}

/// Write one log frame without creating a lossy `Cow` for valid UTF-8.
fn write_frame<W: std::io::Write>(out: &mut W, bytes: &[u8]) -> std::io::Result<()> {
	match std::str::from_utf8(bytes) {
		Ok(text) => out.write_all(text.as_bytes()),
		Err(_) => out.write_all(String::from_utf8_lossy(bytes).as_bytes()),
	}
}

/// Layer the three `run` environment sources into the final `KEY=VALUE` / `KEY`
/// list by precedence (`--env-file` < service `environment:` < `-e`), matching
/// `docker compose run --env-file`. `-e` overrides are appended last so a later
/// duplicate wins downstream, mirroring the previous `-e`-only handling.
fn merge_run_environment(
	env_file_vars: HashMap<String, String>,
	service_env: HashMap<String, Option<String>>,
	env_overrides: Vec<String>,
) -> Vec<String> {
	// `--env-file` is the base layer; the service's `environment:` overrides it.
	let mut map: HashMap<String, Option<String>> = env_file_vars
		.into_iter()
		.map(|(k, v)| (k, Some(v)))
		.collect();
	for (k, v) in service_env {
		map.insert(k, v);
	}
	let mut env_list: Vec<String> = map
		.into_iter()
		.map(|(k, v)| v.map_or_else(|| k.clone(), |v| format!("{k}={v}")))
		.collect();
	// `-e` overrides win over everything else.
	env_list.extend(env_overrides);
	env_list
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;

/// What a non-interactive `run` does when its log stream dies under it.
///
/// The stream arm used to abort the run on any error. A lost chunked terminator
/// is indistinguishable at the transport layer from a real break (#1104), so
/// that failed a `run` whose command had succeeded. The container's state is the
/// out-of-band answer, and `wait` then reports the real exit code.
#[cfg(test)]
#[cfg(unix)]
#[path = "run_stream_end_tests.rs"]
mod stream_end_tests;

/// The board `run` opens over the networks and image it reports before the
/// container's own output takes over (#1671).
#[cfg(all(test, unix))]
#[path = "run_board_tests.rs"]
mod board_tests;
