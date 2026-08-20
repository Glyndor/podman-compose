//! `volumes` — list the named volumes declared by a compose project.
//!
//! Mirrors `docker compose volumes [SERVICE...]`: with no services it lists
//! every top-level `volumes:` entry; with services it lists only the named
//! volumes those services mount. Anonymous/bind mounts are not listed (they
//! have no top-level name), matching docker compose.

use std::collections::BTreeSet;

use crate::compose::types::{ComposeFile, VolumeMount};
use crate::error::Result;
use crate::libpod::types::volume::SystemDf;
use crate::libpod::API_PREFIX;
use crate::units::{format_bytes, SizeFormat};

use super::super::Engine;

/// How `volumes` renders a size: decimal units at three significant digits.
///
/// Three rather than the four `podman system df -v` prints (`193.2MB`,
/// `67.33MB`, measured on Podman 5.7.0). podman is not self-consistent here —
/// `podman images` and `podman ps -s` both use three — and matching podup's own
/// other size columns is worth more than reproducing that split.
const SIZE_FORMAT: SizeFormat = SizeFormat::decimal().with_significant(3);

/// Options for `volumes` added after the crate's API froze.
///
/// `#[non_exhaustive]` from birth: [`VolumesOptions`] is externally
/// constructible with a struct literal, so adding a field to it requires a
/// MAJOR — `cargo semver-checks` reports `constructible_struct_adds_field`.
/// Same reasoning, and the same shape, as `PsDisplayOptions`.
#[derive(Default, Clone, Copy, Debug)]
#[non_exhaustive]
pub struct VolumesDisplayOptions {
	/// Show SIZE and RECLAIMABLE, `-s/--size`.
	///
	/// Off by default and expensive when on: the only endpoint carrying a
	/// volume's size is `system/df`, which accounts for every image, container
	/// and volume on the host. Measured at **1.2 s against 10 ms** for the plain
	/// volume list, on a host with 46 volumes.
	pub size: bool,
}

impl VolumesDisplayOptions {
	/// Ask for the size columns, `-s/--size`. Builder-style.
	#[must_use]
	pub fn with_size(mut self, size: bool) -> Self {
		self.size = size;
		self
	}

	/// The single `volumes` display flag, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next flag to land is not a breaking change for anyone building one.
	pub fn new(size: bool) -> Self {
		Self { size }
	}
}

/// Options for [`Engine::list_volumes`], mirroring `docker compose volumes`.
///
/// `#[non_exhaustive]` since 4.0.0, so a new flag can be added in a minor
/// release without breaking every external caller that built the struct with
/// a literal. Construct it via [`VolumesOptions::new`] or the `with_*` builders
/// below; a struct literal is refused outside this crate, which is what buys
/// the room to grow.
#[derive(Default)]
#[non_exhaustive]
pub struct VolumesOptions {
	/// Print only volume names, `-q/--quiet`.
	pub quiet: bool,
	/// Emit a JSON array instead of the table, `--format json`.
	pub json: bool,
}

impl VolumesOptions {
	/// Every `docker compose volumes` flag, in CLI order. A constructor rather
	/// than a struct literal because the type is `#[non_exhaustive]`, so the
	/// next flag to land is not a breaking change for anyone building one.
	pub fn new(quiet: bool, json: bool) -> Self {
		Self { quiet, json }
	}

	/// Print only volume names, `-q/--quiet`. Builder-style.
	#[must_use]
	pub fn with_quiet(mut self, quiet: bool) -> Self {
		self.quiet = quiet;
		self
	}

	/// Emit a JSON array instead of the table, `--format json`. Builder-style.
	#[must_use]
	pub fn with_json(mut self, json: bool) -> Self {
		self.json = json;
		self
	}
}

impl Engine {
	/// List the project's named volumes (`docker compose volumes`). When
	/// `services` is non-empty, only volumes mounted by those services are shown.
	pub async fn list_volumes(
		&self,
		file: &ComposeFile,
		services: &[String],
		opts: VolumesOptions,
	) -> Result<()> {
		self.list_volumes_with_display(file, services, opts, VolumesDisplayOptions::default())
			.await
	}

	/// Like [`Engine::list_volumes`], plus the options added after the API froze
	/// ([`VolumesDisplayOptions`]). A separate entry point rather than a fourth
	/// parameter on the old one, so existing callers keep compiling.
	pub async fn list_volumes_with_display(
		&self,
		file: &ComposeFile,
		services: &[String],
		opts: VolumesOptions,
		display: VolumesDisplayOptions,
	) -> Result<()> {
		// Reject an unknown service name (docker compose errors with "no such
		// service") instead of silently filtering it out and printing nothing.
		for s in services {
			if !file.services.contains_key(s) {
				return Err(crate::error::ComposeError::ServiceNotFound(s.clone()));
			}
		}
		let keys = self.selected_volume_keys(file, services);

		// (declared key, resolved on-host name, driver, external)
		let rows: Vec<(String, String, String, bool)> = keys
			.iter()
			.map(|key| {
				let cfg = file.volumes.get(key.as_str()).and_then(|c| c.as_ref());
				let external = cfg.and_then(|c| c.external).unwrap_or(false);
				let name = match cfg.and_then(|c| c.name.as_deref()) {
					Some(n) => n.to_string(),
					None if external => key.to_string(),
					None => format!("{}_{}", self.project, key),
				};
				let driver = cfg
					.and_then(|c| c.driver.clone())
					.unwrap_or_else(|| "local".into());
				(key.to_string(), name, driver, external)
			})
			.collect();

		if opts.quiet {
			for (_, name, _, _) in &rows {
				println!("{name}");
			}
			return Ok(());
		}
		// One `system/df` for the whole table when the size was asked for, never
		// per row: the call accounts for the entire host, so asking once per
		// volume would multiply a 1.2 s answer by the number of rows.
		let usage = if display.size {
			self.volume_disk_usage().await?
		} else {
			std::collections::HashMap::new()
		};

		if opts.json {
			let arr: Vec<_> = rows
				.iter()
				.map(|(_, name, driver, external)| {
					// Raw byte counts, and absent entirely when the size was not
					// requested — a consumer can tell "not asked" from "empty",
					// the same distinction the table draws.
					let size = display.size.then(|| {
						let u = usage.get(name.as_str());
						serde_json::json!({
							"Size": u.map(|u| u.size).unwrap_or(0),
							"ReclaimableSize": u.map(|u| u.reclaimable).unwrap_or(0),
							"Links": u.map(|u| u.links).unwrap_or(0),
						})
					});
					serde_json::json!({
						"Name": name,
						"Driver": driver,
						"External": external,
						"Usage": size,
					})
				})
				.collect();
			println!("{}", super::super::to_pretty_json("volumes.row", &arr)?);
			return Ok(());
		}

		// The header prints even with no rows, matching `ps`, `ls`, `images` and
		// `stats`. `volumes` was the only list command that suppressed it, so a
		// script parsing the header line to locate its columns broke on an empty
		// project — and an empty result is a legitimate answer, not an absence of
		// one.
		// EXTERNAL is the most consequential fact about a volume — podup neither
		// creates nor deletes an external one — and the table dropped it while the
		// JSON path above has always carried it. A `down -v` that leaves a volume
		// standing is only explicable if you can see which volumes are external.
		//
		// On `ui::Table` rather than a hand-rolled `{:<40} {:<12}`: cells are
		// escaped and columns sized in one place, so this stops being a third
		// layout dialect that has to be fixed separately every time.
		// EXTERNAL printed `yes`/`no` in plain text while every other meaningful
		// column in the binary carried colour, so the most consequential fact in
		// the table was the least visible one. `caution_col` rather than
		// `status_col`: green would say "healthy", and an external volume is not
		// healthy or unhealthy — it is the one podup will not delete.
		//
		// SIZE and RECLAIMABLE are appended rather than inserted, so a reader's
		// existing column positions do not move when the flag is off.
		let mut headers: Vec<&str> = vec!["NAME", "DRIVER", "EXTERNAL"];
		if display.size {
			headers.push("SIZE");
			headers.push("RECLAIMABLE");
		}
		let mut table = crate::ui::Table::new(&headers)
			.cap(0, 48)
			.identity_col(0)
			.caution_col(2);
		for (_, name, driver, external) in &rows {
			let mut row = vec![
				name.clone(),
				driver.clone(),
				if *external { "yes" } else { "no" }.to_string(),
			];
			if display.size {
				let (size, reclaimable) = size_cells(usage.get(name.as_str()));
				row.push(size);
				row.push(reclaimable);
			}
			table.push(row);
		}
		table.print();
		Ok(())
	}

	/// Per-volume disk usage from `system/df`, keyed by on-host volume name.
	///
	/// One call for the whole table. libpod has no per-volume size endpoint —
	/// this one walks the entire installation — so the cost is paid once or not
	/// at all.
	async fn volume_disk_usage(
		&self,
	) -> Result<std::collections::HashMap<String, crate::libpod::types::volume::VolumeDiskUsage>> {
		let df: SystemDf = self
			.client
			.get_json(&format!("{API_PREFIX}/system/df"))
			.await
			.map_err(crate::error::ComposeError::Podman)?;
		Ok(df
			.volumes
			.into_iter()
			.map(|v| (v.name.clone(), v))
			.collect())
	}

	/// The top-level volume keys to list: all of them, or just those mounted by
	/// `services` (in declaration order), deduplicated.
	fn selected_volume_keys(&self, file: &ComposeFile, services: &[String]) -> Vec<String> {
		if services.is_empty() {
			return file.volumes.keys().cloned().collect();
		}
		let used: BTreeSet<String> = services
			.iter()
			.filter_map(|s| file.services.get(s))
			.flat_map(|svc| svc.volumes.iter().filter_map(mount_source_name))
			.filter(|src| file.volumes.contains_key(src))
			.collect();
		file.volumes
			.keys()
			.filter(|k| used.contains(k.as_str()))
			.cloned()
			.collect()
	}
}

/// The source (named-volume) component of a mount, if any. Bind mounts and
/// anonymous volumes (no source) return `None`.
fn mount_source_name(m: &VolumeMount) -> Option<String> {
	match m {
		VolumeMount::Short(s) => {
			let parts: Vec<&str> = s.splitn(3, ':').collect();
			// `src:target[:opts]` — a leading `.`/`/`/`~` is a bind path, not a name.
			if parts.len() >= 2 && !parts[0].starts_with(['.', '/', '~']) {
				Some(parts[0].to_string())
			} else {
				None
			}
		}
		VolumeMount::Long { source, .. } => source.clone(),
	}
}

#[cfg(test)]
mod tests {
	use super::mount_source_name;
	use crate::compose::types::VolumeMount;

	#[test]
	fn named_volume_short_form_has_source() {
		assert_eq!(
			mount_source_name(&VolumeMount::Short("data:/var/lib".into())),
			Some("data".to_string())
		);
	}

	#[test]
	fn bind_and_anonymous_have_no_source() {
		assert_eq!(
			mount_source_name(&VolumeMount::Short("./host:/c".into())),
			None
		);
		assert_eq!(
			mount_source_name(&VolumeMount::Short("/abs:/c".into())),
			None
		);
		assert_eq!(mount_source_name(&VolumeMount::Short("/data".into())), None);
	}
}

/// The SIZE and RECLAIMABLE cells for one volume.
///
/// A volume the accounting does not mention renders empty rather than `0B`:
/// libpod lists what exists on the host, and a compose file can declare a volume
/// that has never been created. An empty cell says "not there"; `0B` would claim
/// it exists and is empty.
///
/// RECLAIMABLE is shown next to SIZE rather than instead of it because the two
/// answer different questions: a volume still linked by a container reports its
/// full size and **zero** reclaimable, which is the fact someone clearing disk
/// space actually needs.
fn size_cells(usage: Option<&crate::libpod::types::volume::VolumeDiskUsage>) -> (String, String) {
	match usage {
		Some(u) => (
			format_bytes(u.size, &SIZE_FORMAT),
			format_bytes(u.reclaimable, &SIZE_FORMAT),
		),
		None => (String::new(), String::new()),
	}
}

#[cfg(test)]
#[path = "list_tests.rs"]
mod size_tests;
