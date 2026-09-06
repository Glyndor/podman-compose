//! `build.secrets` as the build endpoint wants them.
//!
//! Each referenced top-level secret becomes a file inside the context tar and
//! an `id=NAME,src=ENTRY` spec for the endpoint's `secrets` parameter, so the
//! Dockerfile's `RUN --mount=type=secret,id=NAME` finds it. `external` secrets
//! cannot be forwarded over the API and are warned about and skipped.
use tracing::warn;

use super::ResolvedBuildSecrets;
use crate::compose::types::BuildConfig;
use crate::engine::Engine;
use crate::error::{ComposeError, Result};

impl Engine {
	/// Resolve `build.secrets` into `(in-tar files, secret specs)`.
	///
	/// Each referenced top-level secret is read (from `file:`, inline `content:`,
	/// or `environment:`) and returned as a `(tar-entry-name, bytes)` pair plus a
	/// matching `id=NAME,src=ENTRY` spec for the build endpoint's `secrets` param.
	/// `external` secrets cannot be forwarded over the API and are warned + skipped.
	pub(super) fn resolve_build_secrets(
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
				warn!("build secret '{name}' is external; cannot forward over the libpod build API; skipping");
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
}
