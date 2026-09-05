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
mod service;
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

use crate::compose::types::BuildConfig;
use crate::engine::container_config::resources::is_known_ulimit;
use crate::error::Result;

use stream::ContextSource;

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

/// Resolve the service name list a build pass will iterate over: every service
/// in the file when `target_services` is empty, otherwise the explicit list
/// after validating each name is defined. Returning `Vec<String>` keeps the
/// `build_all_with_options` and `build_images_in_session` paths identical at
/// the top, which is what lets `up --build` share the build's loop body.
fn build_target_names(
	file: &crate::compose::types::ComposeFile,
	target_services: &[String],
) -> Result<Vec<String>> {
	if target_services.is_empty() {
		return Ok(file.services.keys().cloned().collect());
	}
	for name in target_services {
		if !file.services.contains_key(name) {
			return Err(crate::error::ComposeError::ServiceNotFound(name.clone()));
		}
	}
	Ok(target_services.to_vec())
}

/// The image tags `build_images_in_session` will build, in order, deduped.
/// A service without a `build:` block contributes nothing (its row would
/// never move and would read as something hung); two services sharing one
/// image share one row. Pulled out so the same row set can be seeded by
/// `build_all_with_options` and by the `up --build` path in `run_up`.
pub(crate) fn build_image_tags(
	engine: &Engine,
	file: &crate::compose::types::ComposeFile,
	names: &[String],
) -> Vec<String> {
	let mut images: Vec<String> = Vec::new();
	for name in names {
		let Some(service) = file.services.get(name) else {
			continue;
		};
		if service.build.is_none() {
			continue;
		}
		let tag = primary_build_tag(
			&engine.project,
			name,
			service.image.as_deref(),
			service.build.as_ref().map(|b| b.tags()).unwrap_or(&[]),
		);
		if !images.iter().any(|seen| seen == &tag) {
			images.push(tag);
		}
	}
	images
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
		let names = build_target_names(file, target_services)?;
		let images = build_image_tags(self, file, &names);

		crate::ui::progress::begin(
			images
				.iter()
				.map(|image| (crate::ui::progress::Kind::Image, image.clone())),
		);
		let result = self.build_images_in_session(file, &names, opts).await;
		// Close the board on every exit, the way `pull_services_with_options`
		// and `run_up` do. The live region hides the cursor, so an early
		// return through it would leave the terminal without a caret. `end`
		// is idempotent.
		crate::ui::progress::end();
		result
	}

	/// Build the images for `target_services` (every service with a `build:`
	/// block when the list is empty), using an already-open board for the
	/// row events. Unlike [`Self::build_all_with_options`] this does not
	/// seed the board or close it on the way out: the caller is running
	/// inside a board the `up` pass opened, and closing it here would
	/// tear down the very board the rest of `up` is still drawing on.
	///
	/// `image_tags` is the list of image rows the caller already seeded,
	/// in the order they should be built. Passing the same list back is
	/// what lets `build_service` find its row in the board and flip it
	/// from `Pending` to `Working` without re-inserting it.
	pub(in crate::engine) async fn build_images_in_session(
		&self,
		file: &crate::compose::types::ComposeFile,
		target_services: &[String],
		opts: &BuildOptions,
	) -> Result<()> {
		for name in target_services {
			let service = &file.services[name];
			if service.build.is_some() {
				self.build_service(name, service, file, opts).await?;
			}
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
