//! GitHub release source for self-update.
//!
//! Talks to GitHub over HTTPS only (rustls + webpki roots via `ureq`). TLS
//! guards transport, but the downloaded bytes are trusted only after the
//! Ed25519 signature check in [`super::verify`]: a compromised endpoint cannot
//! produce a binary that passes that check.

use std::io::Read;

use crate::ComposeError;

use super::ReleaseSource;

/// `owner/repo` slug of the canonical release repository.
pub const REPO: &str = "Glyndor/podup";

/// Hard cap on any single downloaded asset (defensive against a hostile or
/// broken endpoint streaming unbounded data into memory).
const MAX_ASSET_BYTES: u64 = 128 * 1024 * 1024;

/// Hard cap on the release-metadata JSON response. A `releases/latest` payload
/// is a few KiB; 1 MiB is generous headroom while still bounding memory if a
/// hostile or broken endpoint streams an oversized body.
const MAX_METADATA_BYTES: u64 = 1024 * 1024;

/// Connection-establishment timeout per request.
const CONNECT_TIMEOUT_SECS: u64 = 30;

/// Whole-request timeout (headers + body).
const TOTAL_TIMEOUT_SECS: u64 = 300;

/// Transport failures are retried this many times in total, with exponential
/// backoff (1s, 2s) between attempts. HTTP 4xx responses are not retried;
/// they are deterministic, not transient.
const ATTEMPTS: u32 = 3;

/// Fetches release metadata and assets from GitHub.
pub struct GitHubSource {
	repo: String,
	agent: ureq::Agent,
	/// Base for the releases API (`https://api.github.com`). Overridable in
	/// tests so the transport-error path can be exercised offline.
	api_base: String,
	/// Base for asset downloads (`https://github.com`). Overridable in tests.
	dl_base: String,
}

impl GitHubSource {
	/// Source for the given `owner/repo`.
	pub fn new(repo: impl Into<String>) -> Self {
		let agent: ureq::Agent = ureq::Agent::config_builder()
			.timeout_connect(Some(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS)))
			.timeout_global(Some(std::time::Duration::from_secs(TOTAL_TIMEOUT_SECS)))
			.user_agent(concat!("podup/", env!("CARGO_PKG_VERSION")))
			// Reject any non-HTTPS URL, including a redirect target: GitHub's
			// release download redirects to a CDN, and this prevents that hop (or a
			// hostile one) from being downgraded to plaintext http. The Ed25519
			// signature remains the authenticity gate; this hardens transport.
			.https_only(true)
			.build()
			.into();
		Self {
			repo: repo.into(),
			agent,
			api_base: "https://api.github.com".to_string(),
			dl_base: "https://github.com".to_string(),
		}
	}

	/// Construct with overridden host bases: test seam for the transport-error
	/// path (point at a closed local port to force a connection failure).
	#[cfg(test)]
	fn with_bases(repo: impl Into<String>, api_base: &str, dl_base: &str) -> Self {
		let mut s = Self::new(repo);
		s.api_base = api_base.to_string();
		s.dl_base = dl_base.to_string();
		s
	}

	/// GET `url`, retrying transient transport failures with exponential
	/// backoff (up to [`ATTEMPTS`] tries). Deterministic HTTP 4xx responses are
	/// returned immediately.
	fn get_with_retry(
		&self,
		url: &str,
		accept: Option<&str>,
	) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
		let mut delay = std::time::Duration::from_secs(1);
		let mut attempt = 1;
		loop {
			let mut req = self.agent.get(url);
			if let Some(a) = accept {
				req = req.header("Accept", a);
			}
			match req.call() {
				Ok(resp) => return Ok(resp),
				Err(e) => {
					let deterministic = matches!(e, ureq::Error::StatusCode(code) if code < 500);
					if deterministic || attempt >= ATTEMPTS {
						return Err(e);
					}
					attempt += 1;
					std::thread::sleep(delay);
					delay *= 2;
				}
			}
		}
	}
}

impl Default for GitHubSource {
	fn default() -> Self {
		Self::new(REPO)
	}
}

/// Read at most `cap` bytes from `reader`, erroring if the stream exceeds the
/// cap rather than truncating silently.
fn read_capped(mut reader: impl Read, cap: u64) -> crate::Result<Vec<u8>> {
	let mut buf = Vec::new();
	let read = reader
		.by_ref()
		.take(cap + 1)
		.read_to_end(&mut buf)
		.map_err(ComposeError::Io)?;
	if read as u64 > cap {
		return Err(ComposeError::Update(
			"release data exceeds the maximum allowed size".to_string(),
		));
	}
	Ok(buf)
}

/// Parse the `tag_name` out of a GitHub "latest release" JSON body. Split from
/// the HTTP call so the malformed-metadata failure paths are unit-testable
/// without a network seam.
fn parse_latest_tag(body: &[u8]) -> crate::Result<String> {
	#[derive(serde::Deserialize)]
	struct Latest {
		tag_name: String,
	}
	let latest: Latest = serde_json::from_slice(body)
		.map_err(|e| ComposeError::Update(format!("malformed release metadata: {e}")))?;
	Ok(latest.tag_name)
}

impl ReleaseSource for GitHubSource {
	fn latest_version(&self) -> crate::Result<String> {
		let url = format!("{}/repos/{}/releases/latest", self.api_base, self.repo);
		let resp = self
			.get_with_retry(&url, Some("application/vnd.github+json"))
			.map_err(|e| ComposeError::Update(format!("cannot reach GitHub releases API: {e}")))?;
		let body = read_capped(resp.into_body().into_reader(), MAX_METADATA_BYTES)?;
		parse_latest_tag(&body)
	}

	fn fetch(&self, asset: &str) -> crate::Result<Vec<u8>> {
		// Pinned to the latest release; `ureq` follows GitHub's redirect to the
		// asset host. Always HTTPS; the URL is a compile-time constant scheme.
		let url = format!(
			"{}/{}/releases/latest/download/{asset}",
			self.dl_base, self.repo
		);
		let resp = self
			.get_with_retry(&url, None)
			.map_err(|e| ComposeError::Update(format!("download failed for {asset}: {e}")))?;
		read_capped(resp.into_body().into_reader(), MAX_ASSET_BYTES)
	}
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
