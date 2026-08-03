//! `podup images`: the image behind each service, and what it costs on disk.

use crate::compose::types::ComposeFile;
use crate::error::{ComposeError, Result};
use crate::libpod::types::image::ImageInspect;
use crate::libpod::{urlencoded, API_PREFIX};
use crate::units::{format_bytes, format_duration, DurationFormat, SizeFormat};

use super::inspect_util::split_repo_tag;
use super::Engine;

/// How `images` renders a size: decimal units at three significant digits.
///
/// Decimal because this table exists to be read against `podman images` and
/// `docker compose images`, which are decimal — not against `free`. Three
/// digits because that is what those two print, measured rather than assumed:
/// `docker compose` v5.1.3 rendered `98.2MB` for `redis:8-alpine` and `podman
/// images` rendered `1.01 GB` and `805 kB` on the same host. A fixed decimal
/// count cannot produce all three.
const SIZE_FORMAT: SizeFormat = SizeFormat::decimal().with_significant(3);

/// How `images` renders an age: the shared default, three components.
const AGE_FORMAT: DurationFormat = DurationFormat::default_parts();

/// One row of the `images` table.
///
/// A named struct rather than a tuple because the row has outgrown the point
/// where positional fields stay readable, and because `size` is the raw byte
/// count: the table formats it and the JSON path emits it as a number, so the
/// row has to carry the value rather than a rendering of it.
struct ImageRow {
	service: String,
	repository: String,
	tag: String,
	id: String,
	/// Raw bytes as libpod reported them. Zero when the image is not present
	/// locally, which the table renders as an empty cell — a missing image has
	/// no size, and `0B` would claim it has one.
	size: u64,
	/// The RFC 3339 string libpod sent, kept raw so the table can render an age
	/// and the JSON path can pass the instant through unchanged.
	created: String,
}

impl Engine {
	/// List images used by each service as a table (default options).
	pub async fn images(&self, file: &ComposeFile) -> Result<()> {
		self.images_with_options(file, super::ImagesOptions::default())
			.await
	}

	/// List service images with `docker compose images`-style options:
	/// `-q/--quiet` (IDs only) and `--format` (table | json), across all services.
	/// To restrict to specific services use [`Engine::images_with_services`].
	pub async fn images_with_options(
		&self,
		file: &ComposeFile,
		opts: super::ImagesOptions,
	) -> Result<()> {
		self.images_with_services(file, &[], opts).await
	}

	/// List service images like [`Engine::images_with_options`]. When
	/// `target_services` is non-empty, only those services are listed (an unknown
	/// name is an error), matching `docker compose images [SERVICE...]`.
	pub async fn images_with_services(
		&self,
		file: &ComposeFile,
		target_services: &[String],
		opts: super::ImagesOptions,
	) -> Result<()> {
		for name in target_services {
			if !file.services.contains_key(name) {
				return Err(ComposeError::ServiceNotFound(name.clone()));
			}
		}
		// Collect rows first so quiet/json modes can render without the header.
		let mut rows: Vec<ImageRow> = Vec::new();
		for (name, service) in &file.services {
			if !target_services.is_empty() && !target_services.iter().any(|t| t == name) {
				continue;
			}
			let image_ref = match (&service.image, &service.build) {
				(Some(img), _) => img.clone(),
				// A build-only service's image is the tag the build step produced
				// (project-scoped `{project}-{service}:latest`, or `build.tags[0]`).
				(None, Some(build)) => {
					super::super::build::primary_build_tag(&self.project, name, None, build.tags())
				}
				(None, None) => continue,
			};
			let (repository, tag) = split_repo_tag(&image_ref);
			let path = format!("{API_PREFIX}/images/{}/json", urlencoded(&image_ref));
			match self.client.get_json::<ImageInspect>(&path).await {
				Ok(img) => {
					let id = img.id.trim_start_matches("sha256:").get(..12).unwrap_or("");
					rows.push(ImageRow {
						service: name.clone(),
						repository,
						tag,
						id: id.to_string(),
						size: img.size,
						created: img.created,
					});
				}
				// A 404 means the image is simply not present locally — list it with
				// an empty ID rather than silently dropping it, matching docker
				// compose. Any other error (a connection failure / unreachable
				// socket, or an HTTP 500) is a real failure that must propagate with
				// a non-zero exit rather than printing an empty table and exiting 0.
				Err(e) if e.is_status(404) => {
					tracing::debug!("images {name}: not present ({e})");
					rows.push(ImageRow {
						service: name.clone(),
						repository,
						tag,
						id: String::new(),
						size: 0,
						created: String::new(),
					});
				}
				Err(e) => return Err(ComposeError::Podman(e)),
			}
		}

		if opts.quiet {
			// Deduplicate IDs so services sharing an image emit it once, like
			// docker compose images -q. Empty IDs (not-pulled) are skipped.
			let mut seen = std::collections::HashSet::new();
			for row in &rows {
				if !row.id.is_empty() && seen.insert(row.id.as_str()) {
					println!("{}", row.id);
				}
			}
			return Ok(());
		}
		if opts.json {
			let json: Vec<_> = rows
				.iter()
				.map(|row| {
					// The raw byte count, not the rendered string: this is the
					// machine-facing path, and `docker compose images --format
					// json` emits a number here too (measured against v5.1.3).
					serde_json::json!({
						"Service": row.service,
						"Repository": row.repository,
						"Tag": row.tag,
						"ID": row.id,
						"Size": row.size,
						// The raw instant, like the reference: a machine
						// consumer wants something it can compute with.
						"Created": row.created,
					})
				})
				.collect();
			println!(
				"{}",
				serde_json::to_string_pretty(&json).unwrap_or_default()
			);
			return Ok(());
		}

		// One clock read for the whole table, so two rows built in the same
		// second cannot render different ages.
		let now = super::ps::now_unix();
		let mut table = crate::ui::Table::new(&[
			"SERVICE",
			"REPOSITORY",
			"TAG",
			"IMAGE ID",
			"SIZE",
			"CREATED",
		])
		.cap(0, 48)
		.cap(1, 48)
		.cap(2, 24)
		.identity_col(0);
		for row in &rows {
			table.push(vec![
				row.service.clone(),
				row.repository.clone(),
				row.tag.clone(),
				row.id.clone(),
				size_cell(row.size),
				age_cell(&row.created, now),
			]);
		}
		table.print();
		Ok(())
	}
}

/// The SIZE cell for one row.
///
/// An image that is not present locally has no size to report, so the cell is
/// empty. `0B` would be a claim — that podup asked and the answer was zero —
/// and the row already says the image is missing by carrying no ID.
fn size_cell(size: u64) -> String {
	if size == 0 {
		return String::new();
	}
	format_bytes(size, &SIZE_FORMAT)
}

/// The CREATED cell: how long ago the image was built.
///
/// Empty when libpod sent nothing or something this cannot parse. A blank cell
/// says podup could not tell; a plausible wrong date is the one a reader acts on.
fn age_cell(created: &str, now: i64) -> String {
	let Some(built) = crate::timestamp::parse_rfc3339(created) else {
		return String::new();
	};
	let elapsed = now.saturating_sub(built).max(0);
	format_duration(std::time::Duration::from_secs(elapsed as u64), &AGE_FORMAT)
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
