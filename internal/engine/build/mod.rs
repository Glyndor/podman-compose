//! Image build and pull operations.
//!
//! [`Engine::pull_image`] fetches a pre-built image from a registry.
//! [`Engine::build_service`] compiles a build context tar, passes it to the
//! Podman libpod API, and applies any extra tags. Multi-stage targets are
//! passed as the `target=` query parameter — the full Dockerfile is always sent.

mod context;
mod pull;
mod push;
mod stream;
mod tags;
/// Shared with the `up` image-prefetch stage so it resolves a service's
/// effective pull policy identically to the pull path below.
pub(in crate::engine) use pull::libpod_pull_policy;
pub use pull::PullOptions;
pub use push::PushOptions;
/// Shared with the container-create path so an `up`/`create` references the same
/// image tag the build step produced for a build-only service.
pub(crate) use tags::primary_build_tag;

use bytes::Bytes;
use futures_util::StreamExt;
use tracing::{info, warn};

use crate::compose::types::{BuildConfig, Service};
use crate::error::{ComposeError, Result};
use crate::libpod::types::image::BuildOutput;
use crate::libpod::urlencoded;
use crate::libpod::API_PREFIX;
use crate::size;

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
#[derive(Default, Clone)]
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

		for name in &names {
			let service = &file.services[name];
			if service.build.is_some() {
				self.build_service(name, service, file, opts).await?;
			}
		}
		Ok(())
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
			Some(serde_json::to_string(build.cache_from()).unwrap_or_default())
		};
		let buildargs_json = if build_args.is_empty() {
			None
		} else {
			Some(serde_json::to_string(&build_args).unwrap_or_default())
		};
		let labels_json = if labels.is_empty() {
			None
		} else {
			Some(serde_json::to_string(&labels).unwrap_or_default())
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
		if let Some(target) = build.target() {
			qs.push_str(&format!("&target={}", urlencoded(target)));
		}
		if !secret_specs.is_empty() {
			let json = serde_json::to_string(&secret_specs).unwrap_or_default();
			qs.push_str(&format!("&secrets={}", urlencoded(&json)));
		}
		if !build.cache_to().is_empty() {
			let json = serde_json::to_string(build.cache_to()).unwrap_or_default();
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

		while let Some(result) = stream.next().await {
			match result {
				Ok(output) => {
					// stderr, not stdout. Build output went to stdout via `print!`,
					// which contradicted the documented promise that stdout stays
					// a clean pipe — the one thing a caller redirecting `podup
					// build > log` relies on, and the same promise `config` and
					// `generate quadlet` are built around.
					if !opts.quiet && !output.stream.is_empty() {
						eprint!("{}", output.stream);
					}
					if let Some(err) = output.error_detail.and_then(|e| e.message) {
						return Err(ComposeError::Build(err));
					}
					if let Some(err) = output.error {
						if !err.is_empty() {
							return Err(ComposeError::Build(err));
						}
					}
				}
				Err(e) => return Err(ComposeError::Podman(e)),
			}
		}

		self.apply_extra_tags(build, &tag).await?;
		Ok(())
	}

	/// Resolve `build.secrets` into `(in-tar files, secret specs)`.
	///
	/// Each referenced top-level secret is read (from `file:`, inline `content:`,
	/// or `environment:`) and returned as a `(tar-entry-name, bytes)` pair plus a
	/// matching `id=NAME,src=ENTRY` spec for the build endpoint's `secrets` param.
	/// `external` secrets cannot be forwarded over the API and are warned + skipped.
	fn resolve_build_secrets(
		&self,
		build: &BuildConfig,
		file: &crate::compose::types::ComposeFile,
	) -> Result<ResolvedBuildSecrets> {
		let mut files = Vec::new();
		let mut specs = Vec::new();
		for name in build.secrets() {
			let Some(config) = file.secrets.get(name) else {
				return Err(ComposeError::Unsupported(format!(
					"build secret '{name}' is not defined in the top-level secrets section"
				)));
			};
			let bytes: Vec<u8> = if let Some(host_path) = &config.file {
				// Read through the bounded reader so a hostile or accidentally
				// huge secret file is capped at MAX_FILE_BYTES like every other
				// file read, rather than allocating an unbounded buffer.
				crate::filesystem::read_capped(self.base_dir.join(host_path))
					.map_err(ComposeError::Io)?
			} else if let Some(content) = &config.content {
				content.clone().into_bytes()
			} else if let Some(env_var) = &config.environment {
				std::env::var(env_var)
					.map_err(|_| {
						ComposeError::Unsupported(format!(
							"build secret '{name}' references env var '{env_var}' which is not set"
						))
					})?
					.into_bytes()
			} else if config.external == Some(true) {
				warn!("build secret '{name}' is external; cannot forward over the libpod build API — skipping");
				continue;
			} else {
				continue;
			};
			let entry = format!(".podup-build-secret-{name}");
			specs.push(format!("id={name},src={entry}"));
			files.push((entry, bytes));
		}
		Ok((files, specs))
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
mod tests {
	use super::Engine;
	use crate::libpod::Client;

	fn engine(base: std::path::PathBuf) -> Engine {
		Engine::with_base_dir(Client::new("/nonexistent.sock"), "p".into(), base)
	}

	fn build_of(file: &crate::compose::types::ComposeFile) -> &crate::compose::types::BuildConfig {
		file.services["app"].build.as_ref().unwrap()
	}

	#[test]
	fn build_secret_from_file_shipped_in_tar() {
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("token.txt"), b"s3cr3t").unwrap();
		let yaml = "services:\n  app:\n    build:\n      context: .\n      secrets:\n        - tok\nsecrets:\n  tok:\n    file: token.txt\n";
		let file = crate::compose::parse_str(yaml).unwrap();
		let e = engine(dir.path().to_path_buf());
		let (files, specs) = e.resolve_build_secrets(build_of(&file), &file).unwrap();
		assert_eq!(
			specs,
			vec!["id=tok,src=.podup-build-secret-tok".to_string()]
		);
		assert_eq!(files.len(), 1);
		assert_eq!(files[0].0, ".podup-build-secret-tok");
		assert_eq!(files[0].1, b"s3cr3t");
	}

	#[test]
	fn build_secret_content_inlined() {
		let yaml = "services:\n  app:\n    build:\n      context: .\n      secrets:\n        - c\nsecrets:\n  c:\n    content: inline-value\n";
		let file = crate::compose::parse_str(yaml).unwrap();
		let e = engine(std::env::temp_dir());
		let (files, _) = e.resolve_build_secrets(build_of(&file), &file).unwrap();
		assert_eq!(files[0].1, b"inline-value");
	}

	#[test]
	fn build_secret_external_is_skipped() {
		let yaml = "services:\n  app:\n    build:\n      context: .\n      secrets:\n        - ext\nsecrets:\n  ext:\n    external: true\n";
		let file = crate::compose::parse_str(yaml).unwrap();
		let e = engine(std::env::temp_dir());
		let (files, specs) = e.resolve_build_secrets(build_of(&file), &file).unwrap();
		assert!(files.is_empty());
		assert!(specs.is_empty());
	}

	#[tokio::test]
	async fn empty_build_arg_key_is_rejected() {
		// `--build-arg =value` is a user typo Podman would silently ignore; we
		// reject it before contacting the daemon.
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
		let yaml = "services:\n  app:\n    build:\n      context: .\n";
		let file = crate::compose::parse_str(yaml).unwrap();
		let e = engine(dir.path().to_path_buf());
		let opts = super::BuildOptions {
			build_args: vec!["=orphan".to_string()],
			..Default::default()
		};
		let err = e
			.build_service("app", &file.services["app"], &file, &opts)
			.await
			.expect_err("empty build-arg key must be rejected");
		assert!(
			err.to_string().contains("build-arg"),
			"unexpected error: {err}"
		);
	}

	#[tokio::test]
	async fn invalid_shm_size_is_rejected() {
		// A malformed `build.shm_size` must error rather than silently fall back to
		// the default shm size.
		let dir = tempfile::tempdir().unwrap();
		std::fs::write(dir.path().join("Dockerfile"), b"FROM alpine\n").unwrap();
		let yaml = "services:\n  app:\n    build:\n      context: .\n      shm_size: \"64mb!\"\n";
		let file = crate::compose::parse_str(yaml).unwrap();
		let e = engine(dir.path().to_path_buf());
		let err = e
			.build_service(
				"app",
				&file.services["app"],
				&file,
				&super::BuildOptions::default(),
			)
			.await
			.expect_err("malformed shm_size must be rejected");
		assert!(
			err.to_string().contains("shm_size"),
			"unexpected error: {err}"
		);
	}

	#[test]
	fn build_secret_undefined_errors() {
		let yaml =
			"services:\n  app:\n    build:\n      context: .\n      secrets:\n        - missing\n";
		let file = crate::compose::parse_str(yaml).unwrap();
		let e = engine(std::env::temp_dir());
		assert!(e.resolve_build_secrets(build_of(&file), &file).is_err());
	}
}
