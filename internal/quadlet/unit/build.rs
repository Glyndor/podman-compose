//! Build the `.build` unit for a service that declares a `build:` block.
//!
//! Quadlet's `.build` unit type (a `[Build]` section) tells the systemd
//! generator to build the image before the `.container` unit that consumes it
//! runs. The container references the result via `Image=<stem>.build`.

use std::path::Path;

use crate::compose::types::{BuildConfig, Service};

use super::{owner_marker, sorted_label_pairs, unit_stem, QuadletUnit, Section};

/// Resolve a compose build `context` to an absolute `SetWorkingDirectory` value.
///
/// The systemd generator runs a `.build` unit with no working directory of its
/// own, resolving a relative `SetWorkingDirectory` against the unit file's own
/// directory (`~/.config/containers/systemd`), where there is no Dockerfile. So
/// the context, which compose interprets relative to the compose file, must be
/// made absolute against that base directory here.
///
/// The rule is not specific to a build context: every compose-relative path a
/// generated unit carries needs it, which is why the resolution itself lives in
/// [`abs_against`].
fn abs_context(base_dir: &Path, context: &str) -> String {
	super::abs_against(base_dir, context)
}

/// The `.build` unit file name for a service, e.g. `proj-web.build`. The
/// container unit points its `Image=` at this so Quadlet builds then runs; the
/// stem is project-prefixed so two projects' build units do not clobber each
/// other in the shared systemd directory.
pub(crate) fn build_unit_filename(project: &str, name: &str) -> String {
	format!("{}.build", unit_stem(project, name))
}

/// Whether `service` yields a `.build` unit: it declares `build:` and that
/// build is expressible as Quadlet (an inline Dockerfile is not). Used by the
/// container unit to decide whether `Image=` should reference the `.build`.
pub(crate) fn emits_build_unit(service: &Service) -> bool {
	match &service.build {
		None => false,
		Some(BuildConfig::Context(_)) => true,
		Some(BuildConfig::Config {
			dockerfile_inline, ..
		}) => dockerfile_inline.is_none(),
	}
}

/// Build the `.build` unit for `service`, or `None` when the service has no
/// `build:` block or its build can't be expressed as a Quadlet `.build` unit
/// (an inline Dockerfile, which Quadlet does not support; a warning is pushed).
///
/// `ImageTag=` is the service's `image:` when set, else `<project>-<service>`,
/// so the generated container's `Image=<stem>.build` resolves to a concrete tag.
pub(crate) fn build_unit(
	name: &str,
	project: &str,
	service: &Service,
	base_dir: &Path,
	warnings: &mut Vec<String>,
) -> Option<QuadletUnit> {
	let build = service.build.as_ref()?;

	let mut section = Section::new("Build");

	let image_tag = service
		.image
		.clone()
		.unwrap_or_else(|| format!("{project}-{name}"));
	section.add("ImageTag", image_tag);

	match build {
		BuildConfig::Context(context) => {
			section.add("SetWorkingDirectory", abs_context(base_dir, context));
		}
		BuildConfig::Config {
			context,
			dockerfile,
			dockerfile_inline,
			args,
			target,
			labels,
			network,
			..
		} => {
			if dockerfile_inline.is_some() {
				// Quadlet has no inline-Dockerfile equivalent; emitting a `.build`
				// without the source would build the wrong thing. Skip and warn.
				warnings.push(format!(
					"{name}: build.dockerfile_inline has no Quadlet `.build` equivalent; \
					 no .build unit emitted; build the image first and set `image`"
				));
				return None;
			}
			section.add(
				"SetWorkingDirectory",
				abs_context(base_dir, context.as_deref().unwrap_or(".")),
			);
			if let Some(df) = dockerfile {
				section.add("File", df.clone());
			}
			if let Some(t) = target {
				section.add("Target", t.clone());
			}
			if let Some(net) = network {
				section.add("Network", net.clone());
			}
			let mut build_args: Vec<(String, Option<String>)> = args.to_map().into_iter().collect();
			build_args.sort_by(|a, b| a.0.cmp(&b.0));
			for (key, val) in build_args {
				// `BuildArg=` is not a recognised [Build] Quadlet key (Quadlet would
				// drop the whole unit at daemon-reload), so route build args through
				// PodmanArgs= as `--build-arg`, like the container CPU/memory limits.
				match val {
					Some(v) => section.add("PodmanArgs", format!("--build-arg {key}={v}")),
					None => section.add("PodmanArgs", format!("--build-arg {key}")),
				}
			}
			for (key, val) in sorted_label_pairs(labels.to_map()) {
				section.add("Label", format!("{key}={val}"));
			}
		}
	}

	// Ownership label, mirroring every other generated unit.
	section.add("Label", format!("podup.project={project}"));

	// The unforgeable ownership marker comes first, as its own comment line;
	// see `owner_marker` for why it must stay separate from the `Label=` line.
	let mut contents = owner_marker(project);
	contents.push_str(&section.render());

	Some(QuadletUnit {
		filename: build_unit_filename(project, name),
		contents,
	})
}

// Unix-gated: the asserted paths are POSIX (separators and `/`-absolute), so the
// values differ on Windows. `abs_context` itself is cross-platform; only the
// literal expectations are Unix-specific.
#[cfg(all(test, unix))]
#[path = "build_tests.rs"]
mod tests;
