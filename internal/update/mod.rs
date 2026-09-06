//! Secure self-update for the `podup` binary.
//!
//! Flow: resolve the latest release tag, compare against the compiled-in
//! version, and (unless `--check`) download the platform binary plus the signed
//! `SHA256SUMS` manifest. The manifest's Ed25519 signature is verified against
//! the public keys embedded in this binary (`verify::RELEASE_PUBKEYS`); only
//! then is the binary's digest checked against the manifest and the running
//! executable atomically replaced. Every step fails closed: a missing key,
//! bad signature, or checksum mismatch aborts before anything is written.

mod github;
mod install;
mod pkg;
mod verify;

pub use github::{GitHubSource, REPO};

use crate::ComposeError;

/// Options controlling an update run.
#[derive(Debug, Clone, Copy, Default)]
pub struct UpdateOptions {
	/// Report whether a newer release exists without installing it.
	pub check_only: bool,
	/// Reinstall even if the latest release is not newer than the current build.
	pub force: bool,
}

/// A source of release metadata and assets. Abstracted so the verification and
/// install flow can be exercised without network access.
pub trait ReleaseSource {
	/// Latest published release tag (e.g. `v0.6.0`).
	fn latest_version(&self) -> crate::Result<String>;
	/// Raw bytes of a named release asset.
	fn fetch(&self, asset: &str) -> crate::Result<Vec<u8>>;
}

/// Run an update against GitHub for the version compiled into this binary.
pub fn run(opts: UpdateOptions) -> crate::Result<()> {
	// Best-effort cleanup of a `.old` backup a prior Windows swap could not
	// delete immediately (the old process still held it open). By the time
	// any updater run starts, that process has exited, so this is the
	// earliest point the leftover can go - see the module doc on
	// `install::swap_into_place`.
	#[cfg(windows)]
	install::cleanup_stale_backup();

	let source = GitHubSource::default();
	run_with(&source, env!("CARGO_PKG_VERSION"), opts)
}

/// Core update flow against an arbitrary [`ReleaseSource`] and current version.
pub fn run_with(
	source: &dyn ReleaseSource,
	current: &str,
	opts: UpdateOptions,
) -> crate::Result<()> {
	run_with_guard(source, current, opts, pkg::managing_package_manager())
}

/// [`run_with`] with the package-manager guard injected, so the refusal branch
/// can be exercised without a dpkg-managed binary on the test host.
fn run_with_guard(
	source: &dyn ReleaseSource,
	current: &str,
	opts: UpdateOptions,
	managed_by: Option<&str>,
) -> crate::Result<()> {
	let current_v = verify::parse_version(current)?;
	let latest_tag = source.latest_version()?;
	let latest_v = verify::parse_version(&latest_tag)?;

	if latest_v <= current_v && !opts.force {
		println!("podup is up to date (v{current})");
		return Ok(());
	}

	if latest_v > current_v {
		println!("update available: v{current} -> {latest_tag}");
	} else {
		println!("reinstalling {latest_tag} (--force)");
	}
	if opts.check_only {
		println!("run `podup update` to install it");
		return Ok(());
	}

	// Refuse to self-replace a package-manager-managed binary (even with
	// --force): overwriting it would desync the package manager's records.
	if let Some(pm) = managed_by {
		return Err(pkg::package_managed_error(pm));
	}

	let asset = install::require_platform_asset()?;

	// Security gate: fetch and verify the signed manifest *before* downloading the
	// binary, so a tampered/unsigned release is rejected without first buffering a
	// large attacker-controlled payload. The binary's digest is then checked
	// against the verified manifest (fail-closed).
	let sha256sums = source.fetch("SHA256SUMS")?;
	let signature = source.fetch("SHA256SUMS.sig")?;
	verify::verify_signature(&sha256sums, &signature)?;
	let expected = verify::expected_digest(&sha256sums, asset)?;

	println!("downloading {asset} ({latest_tag}) ...");
	let binary = source.fetch(asset)?;
	verify::verify_digest(&binary, &expected)?;
	println!("signature and checksum verified");

	// The self-test inside `install_binary` pins the installed binary's reported
	// version to the resolved tag, closing the signed-release rollback window.
	let path = install::install_binary(&binary, latest_tag.trim_start_matches('v'))?;
	println!("updated to {latest_tag}: {}", path.display());
	Ok(())
}

/// Stable process exit code for an update failure. Distinct from clap's
/// reserved `2` (usage errors) and from `1` (generic failure), so scripts can
/// branch reliably on "update failed".
pub const UPDATE_FAILURE_EXIT_CODE: i32 = 3;

/// Map an update failure onto its stable process exit code
/// ([`UPDATE_FAILURE_EXIT_CODE`]), distinct from a run-container exit, so
/// scripts can branch on "update failed".
pub fn exit_code(_err: &ComposeError) -> i32 {
	UPDATE_FAILURE_EXIT_CODE
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
