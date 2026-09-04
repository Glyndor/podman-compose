//! Image build and pull operations.
//!
//! [`Engine::pull_image`] fetches a pre-built image from a registry.
//! [`Engine::build_service`] compiles a build context tar, passes it to the
//! Podman libpod API, and applies any extra tags. Multi-stage targets are
//! passed as the `target=` query parameter — the full Dockerfile is always sent.

mod context;
mod pull;
mod push;
mod secrets;
mod steps;
mod stream;
mod tags;
/// Shared with the `up` image-prefetch and `up`/pull decision paths so they
/// resolve a service's effective pull policy identically to the pull path
/// below, and reject an unrecognized value the same way (#1443).
pub(in crate::engine) use pull::pull_policy_checked;
pub use pull::PullOptions;
pub use push::PushOptions;
/// Shared with the container-create path so an `up`/`create` references the same
/// image tag the build step produced for a build-only service.
pub(crate) use tags::primary_build_tag;

use bytes::Bytes;
use futures_util::StreamExt;
use tracing::{info, warn};

use std::io::IsTerminal;

use crate::compose::types::{BuildConfig, Service};
use crate::engine::container_config::resources::is_known_ulimit;
use crate::error::{ComposeError, Result};
use crate::libpod::types::image::BuildOutput;
use crate::libpod::urlencoded;
use crate::libpod::validate::pre_validate_build;
use crate::libpod::API_PREFIX;
use crate::size;
use steps::{parse_image_id_line, BuildStreamProgress};

use context::{map_additional_context, INLINE_DOCKERFILE_NAME};
use stream::{context_body, ContextSource};
use tags::{is_remote_context, looks_like_secret};

use super::Engine;

/// Files to ship inside the build-context tar plus their matching `secrets=`
/// specs (`id=NAME,src=ENTRY`) for the libpod build endpoint.
type ResolvedBuildSecrets = (Vec<(String, Vec<u8>)>, Vec<String>);

/// How `build_service` sends the context to the libpod build endpoint.
enum BodyPlan {
	/// A Git/URL context — Podman clones it server-side, so the body is empty.
	Empty,
	/// A local context, streamed to the socket from a `spawn_blocking` tar writer
	/// so its size never drives the process's RSS.
	Stream {
		context: std::path::PathBuf,
		source: ContextSource,
		secrets: Vec<(String, Vec<u8>)>,
	},
}

/// `docker compose build`-style CLI overrides. Each augments (never weakens)
/// the per-service `build:` config: a flag forces the behaviour on even when
/// the compose file leaves it off.
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`BuildOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Default, Clone)]
#[non_exhaustive]
pub struct BuildOptions {
	/// Force a cache-less build (`--no-cache`).
	pub no_cache: bool,
	/// Always attempt to pull a newer base image (`--pull`).
	pub pull: bool,
	/// Extra build args (`KEY=VAL`); override the compose `build.args` on conflict.
	pub build_args: Vec<String>,
	/// Suppress build output (`-q/--quiet`).
	pub quiet: bool,
}

impl BuildOptions {
	/// Every `docker compose build` flag, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next flag to land is not a breaking change for anyone building one.
	pub fn new(no_cache: bool, pull: bool, build_args: Vec<String>, quiet: bool) -> Self {
		Self {
			no_cache,
			pull,
			build_args,
			quiet,
		}
	}

	/// Force a cache-less build (`--no-cache`). Builder-style.
	#[must_use]
	pub fn with_no_cache(mut self, no_cache: bool) -> Self {
		self.no_cache = no_cache;
		self
	}

	/// Always attempt to pull a newer base image (`--pull`). Builder-style.
	#[must_use]
	pub fn with_pull(mut self, pull: bool) -> Self {
		self.pull = pull;
		self
	}

	/// Extra build args (`KEY=VAL`); override the compose `build.args` on
	/// conflict. Builder-style.
	#[must_use]
	pub fn with_build_args(mut self, build_args: Vec<String>) -> Self {
		self.build_args = build_args;
		self
	}

	/// Suppress build output (`-q/--quiet`). Builder-style.
	#[must_use]
	pub fn with_quiet(mut self, quiet: bool) -> Self {
		self.quiet = quiet;
		self
	}
}

/// Render `build.ulimits` into the `name=soft:hard` strings the libpod build
/// endpoint takes.
///
/// podup used to report this field as having no libpod mapping. It does.
/// Measured on podman 5.7.0 by building the same Containerfile twice through
/// the API, with a `RUN` that prints `ulimit -n`: without the parameter the
/// build saw 524288, with `ulimits=["nofile=1234:1234"]` it saw 1234.
///
/// An unrecognised name is dropped rather than forwarded. The value reaches a
/// query string, so an unknown name is either a typo or an injection attempt —
/// the same reasoning the container-side `build_ulimits` applies.
fn render_build_ulimits(build: &BuildConfig) -> Vec<String> {
	build
		.ulimits()
		.iter()
		.filter_map(|(name, cfg)| {
			if !is_known_ulimit(name) {
				tracing::warn!(
					"build.ulimits '{name}' is not a recognized resource name and is ignored"
				);
				return None;
			}
			let (soft, hard) = (cfg.soft(), cfg.hard());
			// Podman rejects a soft limit above the hard one; the container
			// path clamps rather than failing the build, so match it.
			let soft = soft.min(hard);
			Some(format!("{name}={soft}:{hard}"))
		})
		.collect()
}

impl Engine {
	/// Build (or rebuild) images for services that have a `build:` block.
	///
	/// If `target_services` is empty, every service with a build config is built.
	/// Services without a build config are silently skipped.
	pub async fn build_all(
		&self,
		file: &crate::compose::types::ComposeFile,
		target_services: &[String],
	) -> Result<()> {
		self.build_all_with_options(file, target_services, &BuildOptions::default())
			.await
	}

	/// Build service images with `docker compose build`-style overrides
	/// (`--no-cache`, `--pull`, `--build-arg`, `--quiet`).
	pub async fn build_all_with_options(
		&self,
		file: &crate::compose::types::ComposeFile,
		target_services: &[String],
		opts: &BuildOptions,
	) -> Result<()> {
		let names: Vec<String> = if target_services.is_empty() {
			file.services.keys().cloned().collect()
		} else {
			for name in target_services {
				if !file.services.contains_key(name) {
					return Err(crate::error::ComposeError::ServiceNotFound(name.clone()));
				}
			}
			target_services.to_vec()
		};

		// Seed the board with the image set this pass will build, one row
		// per image, before the first `Building` line. A service with no
		// `build:` block is skipped, the same way `build_service` does; its
		// row would never move and would read as something hung (#1681).
		// Two services on the same image share one row, since the row is
		// the image.
		let mut images: Vec<String> = Vec::new();
		for name in &names {
			let Some(service) = file.services.get(name) else {
				continue;
			};
			if service.build.is_none() {
				continue;
			}
			let tag = primary_build_tag(
				&self.project,
				name,
				service.image.as_deref(),
				service.build.as_ref().map(|b| b.tags()).unwrap_or(&[]),
			);
			if !images.iter().any(|seen| seen == &tag) {
				images.push(tag);
			}
		}
		crate::ui::progress::begin(
			images
				.iter()
				.map(|image| (crate::ui::progress::Kind::Image, image.clone())),
		);

		let result = async {
			for name in &names {
				let service = &file.services[name];
				if service.build.is_some() {
					self.build_service(name, service, file, opts).await?;
				}
			}
			Ok(())
		}
		.await;

		// Close the board on every exit, the way `pull_services_with_options`
		// and `run_up` do. The live region hides the cursor, so an early
		// return through it would leave the terminal without a caret. `end`
		// is idempotent.
		crate::ui::progress::end();
		result
	}

	pub(super) async fn build_service(
		&self,
		service_name: &str,
		service: &Service,
		file: &crate::compose::types::ComposeFile,
		opts: &BuildOptions,
	) -> Result<()> {
		let build = match &service.build {
			Some(b) => b,
			None => return Ok(()),
		};

		let context_str = build.context().to_string();
		let remote_context = is_remote_context(&context_str);
		let tag = primary_build_tag(
			&self.project,
			service_name,
			service.image.as_deref(),
			build.tags(),
		);

		// A Git/URL context is cloned server-side by Podman via the `remote`
		// query parameter — there is no local directory to tar. Tar-only features
		// (inline Dockerfile, in-tar build secrets) do not apply.
		let (body_plan, dockerfile_name, secret_specs) = if remote_context {
			info!("building {tag} from remote context {context_str}");
			if build.dockerfile_inline().is_some() {
				warn!("build.dockerfile_inline is ignored for a remote build context");
			}
			if !build.secrets().is_empty() {
				warn!("build.secrets are ignored for a remote build context");
			}
			let df = build.dockerfile().unwrap_or("Dockerfile").to_string();
			(BodyPlan::Empty, df, Vec::new())
		} else {
			let context_path = self.base_dir.join(&context_str);
			// Fail fast with the service name and the resolved context path if the
			// directory is missing/unreadable, instead of a bare "io error: No such
			// file or directory" once the context walk hits it.
			if let Err(e) = std::fs::metadata(&context_path) {
				return Err(ComposeError::BuildContext {
					service: service_name.to_string(),
					path: context_path.display().to_string(),
					source: e,
				});
			}
			info!("building {tag} from {}", context_path.display());

			// Resolve `build.secrets` to in-tar files before building the context:
			// each secret value is shipped inside the build-context tar and
			// referenced by a relative `src=` path, which is the form the libpod
			// build endpoint expects (`env=`/host-path forms don't work reliably
			// over the socket).
			let (secret_files, secret_specs) = self.resolve_build_secrets(build, file)?;

			// The context tar is streamed to the socket (see the POST below), never
			// buffered, so a multi-gigabyte context doesn't inflate RSS. Decide the
			// source and the dockerfile name here; the blocking tar walk happens
			// while the request body is being sent.
			let (source, dockerfile_name) = match build.dockerfile_inline() {
				Some(inline) => (
					ContextSource::Inline(inline.to_string()),
					INLINE_DOCKERFILE_NAME.to_string(),
				),
				None => {
					// Honour an explicit dockerfile; otherwise prefer Dockerfile
					// but fall back to Podman's native Containerfile when only the
					// latter is present.
					let df = match build.dockerfile() {
						Some(name) => name.to_string(),
						None if !context_path.join("Dockerfile").is_file()
							&& context_path.join("Containerfile").is_file() =>
						{
							"Containerfile".to_string()
						}
						None => "Dockerfile".to_string(),
					};
					(ContextSource::Dockerfile(df.clone()), df)
				}
			};
			(
				BodyPlan::Stream {
					context: context_path,
					source,
					secrets: secret_files,
				},
				dockerfile_name,
				secret_specs,
			)
		};

		let arg_map = build.args().to_map();
		let mut build_args: std::collections::HashMap<String, String> =
			std::collections::HashMap::new();
		for (k, v) in arg_map {
			let value = v.unwrap_or_else(|| std::env::var(&k).unwrap_or_default());
			build_args.insert(k, value);
		}
		// CLI `--build-arg KEY=VAL` overrides the compose `build.args`. A bare
		// `KEY` (no `=`) takes its value from the process environment, matching
		// docker compose.
		for entry in &opts.build_args {
			let (k, v) = match entry.split_once('=') {
				Some((k, v)) => (k.to_string(), v.to_string()),
				None => (entry.clone(), std::env::var(entry).unwrap_or_default()),
			};
			// A bare `=value` (empty key) is a user typo Podman would silently
			// ignore; reject it with a clear diagnostic instead.
			if k.is_empty() {
				return Err(ComposeError::Build(format!(
					"invalid --build-arg '{entry}': empty argument name"
				)));
			}
			build_args.insert(k, v);
		}

		// A secret passed as a build arg is recorded in the image history and, if
		// promoted via `ENV`, the image config — so it can leak. Warn and point at
		// `build.secrets` (BuildKit `--mount=type=secret`), which does not persist.
		let mut secretish: Vec<&str> = build_args
			.keys()
			.filter(|k| looks_like_secret(k))
			.map(String::as_str)
			.collect();
		if !secretish.is_empty() {
			secretish.sort_unstable();
			warn!(
				"build-arg(s) [{}] look like secrets; build args are stored in the image history \
				 and can leak. Use build.secrets for sensitive values.",
				secretish.join(", ")
			);
		}

		let mut labels: std::collections::HashMap<String, String> =
			std::collections::HashMap::new();
		if let BuildConfig::Config { labels: l, .. } = build {
			labels.extend(l.to_map());
		}

		// Pre-validate the keys libpod's buildkit-fronted parser rejects
		// (anything outside `[A-Za-z0-9_.-]`), so a bad `build.args` or
		// `build.labels` key surfaces as a `PodmanError::Field` naming the
		// compose-side field rather than libpod's `400` body. Runs before
		// the build URL is assembled so a bad key fails before any POST to
		// the daemon (#1357).
		pre_validate_build(&build_args, &labels)?;

		let network = if let BuildConfig::Config {
			network: Some(n), ..
		} = build
		{
			Some(n.clone())
		} else {
			None
		};
		let platform = match build {
			BuildConfig::Config { platforms, .. } => platforms.first().inspect(|first| {
				let rest_count = platforms.len() - 1;
				if rest_count > 0 {
					warn!("build.platforms: libpod builds one platform per request; building {first}, ignoring {rest_count} other(s)");
				}
			}).cloned(),
			_ => None,
		};
		// Distinguish "absent" from "present but unparseable": a malformed
		// `shm_size` must be rejected, not silently dropped to the default.
		let shmsize = match build.shm_size() {
			Some(raw) => Some(size::parse_memory(raw).ok_or_else(|| {
				ComposeError::Build(format!("invalid build.shm_size value '{raw}'"))
			})? as i32),
			None => None,
		};
		let extrahosts_str = build.extra_hosts().join(",");
		let extrahosts = if extrahosts_str.is_empty() {
			None
		} else {
			Some(extrahosts_str)
		};
		let cachefrom = if build.cache_from().is_empty() {
			None
		} else {
			Some(super::to_query_json(
				"build.cache_from",
				&build.cache_from(),
			)?)
		};
		let buildargs_json = if build_args.is_empty() {
			None
		} else {
			Some(super::to_query_json("build.args", &build_args)?)
		};
		let labels_json = if labels.is_empty() {
			None
		} else {
			Some(super::to_query_json("build.labels", &labels)?)
		};
		let build_ulimits = render_build_ulimits(build);
		let ulimits_json = if build_ulimits.is_empty() {
			None
		} else {
			Some(super::to_query_json("build.ulimits", &build_ulimits)?)
		};

		let mut qs = format!(
			"t={}&rm=true&nocache={}",
			urlencoded(&tag),
			build.no_cache() || opts.no_cache
		);
		qs.push_str(&format!("&dockerfile={}", urlencoded(&dockerfile_name)));
		if build.pull() || opts.pull {
			qs.push_str("&pull=true");
		}
		if let Some(p) = &platform {
			qs.push_str(&format!("&platform={}", urlencoded(p)));
		}
		if let Some(n) = &network {
			qs.push_str(&format!("&networkmode={}", urlencoded(n)));
		}
		if let Some(s) = shmsize {
			qs.push_str(&format!("&shmsize={s}"));
		}
		if let Some(h) = &extrahosts {
			qs.push_str(&format!("&extrahosts={}", urlencoded(h)));
		}
		if let Some(c) = &cachefrom {
			qs.push_str(&format!("&cachefrom={}", urlencoded(c)));
		}
		if let Some(a) = &buildargs_json {
			qs.push_str(&format!("&buildargs={}", urlencoded(a)));
		}
		if let Some(l) = &labels_json {
			qs.push_str(&format!("&labels={}", urlencoded(l)));
		}
		if let Some(u) = &ulimits_json {
			qs.push_str(&format!("&ulimits={}", urlencoded(u)));
		}
		if let Some(target) = build.target() {
			qs.push_str(&format!("&target={}", urlencoded(target)));
		}
		if !secret_specs.is_empty() {
			let json = super::to_query_json("build.secrets", &secret_specs)?;
			qs.push_str(&format!("&secrets={}", urlencoded(&json)));
		}
		if !build.cache_to().is_empty() {
			let json = super::to_query_json("build.cache_to", &build.cache_to())?;
			qs.push_str(&format!("&cacheto={}", urlencoded(&json)));
		}
		for (name, value) in build.additional_contexts() {
			let mapped = map_additional_context(&self.base_dir, &value);
			qs.push_str(&format!(
				"&additionalbuildcontexts={}",
				urlencoded(&format!("{name}={mapped}"))
			));
		}
		if !build.ssh().is_empty() {
			warn!(
				"build.ssh is not supported over the libpod REST build API; ignoring {:?}",
				build.ssh()
			);
		}

		if remote_context {
			qs.push_str(&format!("&remote={}", urlencoded(&context_str)));
		}

		let path = format!("{API_PREFIX}/build?{qs}");
		let resp = match body_plan {
			BodyPlan::Empty => self
				.client
				.post_bytes_stream(&path, Bytes::new(), "application/x-tar")
				.await
				.map_err(ComposeError::Podman)?,
			BodyPlan::Stream {
				context,
				source,
				secrets,
			} => {
				// The tar writer runs on a blocking thread feeding the request
				// body; drain the body first, then join the writer — joining
				// before the body is drained would deadlock on the bounded
				// channel. If the request itself failed, that transport error is
				// the real cause; otherwise surface any context-assembly error.
				let (producer, body) = context_body(context, source, secrets);
				let sent = self
					.client
					.post_stream_body(&path, body, "application/x-tar")
					.await;
				let produced = producer
					.await
					.map_err(|e| ComposeError::Build(e.to_string()))?;
				match sent {
					Ok(resp) => {
						produced?;
						resp
					}
					Err(e) => return Err(ComposeError::Podman(e)),
				}
			}
		};
		let mut stream = crate::libpod::parse_json_lines::<BuildOutput>(resp.into_body());

		// Open the board row for this image before the first `STEP` line, so
		// the row's `Building` verb is what the reader sees while the stream
		// arrives. `progress::start` is a no-op when the board already had
		// the row seeded (`build_all_with_options` did this for a standalone
		// `build`), and inserts it before the service's container row when
		// `up --build` calls into a board the `up` pass opened. Either way,
		// the row ends up with the right identifier, in the right position
		// and with a working verb (#1681).
		if !opts.quiet {
			let first_container = self.replica_names(service_name, service).into_iter().next();
			match first_container {
				Some(container_name) => crate::ui::progress::start_anchored(
					"Image",
					&tag,
					"Building",
					Some("Container"),
					Some(&container_name),
				),
				None => crate::ui::progress::start("Image", &tag, "Building"),
			}
		}

		// The captured stream of this image: needed on a terminal failure
		// path, where the notes buffer only kept the last 4 lines and the
		// rest has to be replayed as scrollback so the reason is on screen.
		// Off a terminal, every line is already in stderr through `note_for`,
		// so this stays empty in the test runs.
		let mut capture: Vec<String> = Vec::new();
		// Track the last image id the stream emitted, so the success path
		// can put it on stdout when stdout is not a terminal. Buildah
		// closes with a `--> sha256:<64-hex>` line, which is what a script
		// capturing stdout wants; on a terminal it is dropped (the row
		// already says it landed). `None` until the first matching line.
		let mut last_image_id: Option<String> = None;
		let mut progress = BuildStreamProgress::new();

		while let Some(result) = stream.next().await {
			match result {
				Ok(output) => {
					if !output.stream.is_empty() {
						let trimmed = output.stream.trim_end().to_string();
						if !opts.quiet && !trimmed.is_empty() {
							// `STEP n/m:` lines advance the row verb. Every
							// other line is a tail note, painted dimmed under
							// the row on a terminal and prefixed on stderr in
							// a pipe.
							if let Some(verb) = progress.observe(&trimmed) {
								crate::ui::progress::start("Image", &tag, &verb);
							}
							// The image id line is the one carry-over from
							// today that a script reading stdout needs. It
							// goes to notes/live as any other line, and is
							// additionally stashed so the success path can
							// echo it to stdout when stdout is not a tty.
							if let Some(id) = parse_image_id_line(&trimmed) {
								last_image_id = Some(id);
							}
							crate::ui::progress::note_for("Image", &tag, &trimmed);
						}
						capture.push(trimmed);
					}
					if let Some(err) = output.error_detail.and_then(|e| e.message) {
						return Err(self.fail_build(&tag, err, capture, opts.quiet));
					}
					if let Some(err) = output.error {
						if !err.is_empty() {
							return Err(self.fail_build(&tag, err, capture, opts.quiet));
						}
					}
				}
				Err(e) => return Err(ComposeError::Podman(e)),
			}
		}

		// Success: close the row with `Built`. The trailing image id goes to
		// stdout only when stdout is not a terminal, so a script reading
		// `podup build | awk '{print $1}'` can still pluck the id; on a
		// terminal the row says it landed and the id would only be noise.
		if !opts.quiet {
			crate::ui::progress_line("Image", &tag, "Built");
		}
		if !std::io::stdout().is_terminal() {
			let resolved = match last_image_id {
				Some(id) => Some(id),
				None => match self.image_id(&tag).await {
					Ok(Some(id)) => Some(id),
					_ => None,
				},
			};
			if let Some(id) = resolved {
				use std::io::Write;
				let _ = writeln!(std::io::stdout(), "{id}");
			}
		}

		self.apply_extra_tags(build, &tag).await?;
		Ok(())
	}

	/// Apply any `build.tags` aliases to the freshly built image.
	///
	/// The primary `tag` is skipped: when no `image:` is set it is already
	/// `tags[0]`, which the build itself produced, so re-tagging it onto itself
	/// would be a no-op API call.
	async fn apply_extra_tags(&self, build: &BuildConfig, tag: &str) -> Result<()> {
		for extra_tag in build.tags() {
			if extra_tag == tag {
				continue;
			}
			let (repo, tag_str) = extra_tag
				.rsplit_once(':')
				.map(|(r, t)| (r.to_string(), t.to_string()))
				.unwrap_or_else(|| (extra_tag.clone(), "latest".to_string()));
			let encoded_tag = urlencoded(tag);
			let tag_path = format!(
				"{API_PREFIX}/images/{encoded_tag}/tag?repo={}&tag={}",
				urlencoded(&repo),
				urlencoded(&tag_str),
			);
			// Returning () here meant `build` could not report a failed tag at
			// all: it exited 0 with the requested tags missing.
			self.client
				.post_empty_ok(&tag_path)
				.await
				.map_err(ComposeError::Podman)?;
		}
		Ok(())
	}
}

#[cfg(test)]
#[path = "build_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "build_board_tests.rs"]
mod board_tests;
