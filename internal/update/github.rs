//! GitHub release source for self-update.
//!
//! Talks to GitHub over HTTPS only (rustls + webpki roots via `ureq`). TLS
//! guards transport, but the downloaded bytes are trusted only after the
//! Ed25519 signature check in [`super::verify`] — a compromised endpoint cannot
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
/// backoff (1s, 2s) between attempts. HTTP 4xx responses are not retried —
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

	/// Construct with overridden host bases — test seam for the transport-error
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
		// asset host. Always HTTPS — the URL is a compile-time constant scheme.
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
mod tests {
	use super::*;

	/// A reader that yields zero bytes forever — used to exercise the cap.
	struct Endless;
	impl Read for Endless {
		fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
			for b in buf.iter_mut() {
				*b = 0;
			}
			Ok(buf.len())
		}
	}

	#[test]
	fn read_capped_accepts_small() {
		let data = b"hello world".to_vec();
		let got = read_capped(&data[..], MAX_ASSET_BYTES).unwrap();
		assert_eq!(got, data);
	}

	#[test]
	fn read_capped_rejects_oversize() {
		assert!(read_capped(Endless, MAX_ASSET_BYTES).is_err());
	}

	#[test]
	fn read_capped_enforces_metadata_cap() {
		// The metadata cap is far smaller than the asset cap; an endless stream
		// must be rejected once it crosses the 1 MiB metadata bound.
		assert!(read_capped(Endless, MAX_METADATA_BYTES).is_err());
	}

	#[test]
	fn read_capped_accepts_up_to_cap() {
		// Exactly `cap` bytes is allowed; cap+1 is rejected.
		let exactly = [0u8; 8];
		assert!(read_capped(&exactly[..], 8).is_ok());
		let over = [0u8; 9];
		assert!(read_capped(&over[..], 8).is_err());
	}

	#[test]
	fn default_uses_canonical_repo() {
		let src = GitHubSource::default();
		assert_eq!(src.repo, REPO);
	}

	#[test]
	fn parse_latest_tag_extracts_tag() {
		let tag = parse_latest_tag(br#"{"tag_name":"v1.2.3","name":"r"}"#).unwrap();
		assert_eq!(tag, "v1.2.3");
	}

	#[test]
	fn parse_latest_tag_rejects_malformed_json() {
		assert!(parse_latest_tag(b"not json at all").is_err());
		assert!(parse_latest_tag(b"").is_err());
	}

	#[test]
	fn parse_latest_tag_rejects_missing_field() {
		// Well-formed JSON object without `tag_name` must fail, not default.
		let err = parse_latest_tag(br#"{"name":"release"}"#).unwrap_err();
		assert!(err.to_string().contains("malformed release metadata"));
	}

	#[test]
	fn latest_version_maps_transport_error() {
		// https so the request reaches a socket. Port 1 is closed, so the failure
		// is a connection refusal: offline, deterministic, and genuinely the
		// transport path this test is named for. An http base would never get
		// that far — the agent rejects the scheme first, which is what this test
		// used to measure while claiming to measure the socket.
		use crate::update::ReleaseSource;
		let src = GitHubSource::with_bases(REPO, "https://127.0.0.1:1", "https://127.0.0.1:1");
		let err = src.latest_version().unwrap_err();
		assert!(
			err.to_string().contains("cannot reach GitHub releases API"),
			"got: {err}"
		);
		assert!(
			err.to_string().contains("Connection refused"),
			"the transport path must be the one that failed, got: {err}"
		);
	}

	#[test]
	fn fetch_maps_transport_error() {
		use crate::update::ReleaseSource;
		let src = GitHubSource::with_bases(REPO, "https://127.0.0.1:1", "https://127.0.0.1:1");
		let err = src.fetch("podup-linux-x86_64").unwrap_err();
		assert!(err.to_string().contains("download failed"), "got: {err}");
		assert!(
			err.to_string().contains("Connection refused"),
			"the transport path must be the one that failed, got: {err}"
		);
	}

	/// A plaintext base must be refused for being plaintext, not for anything
	/// else. `https_only(true)` is the only thing standing between a hostile
	/// redirect and a downgraded download, and until this test existed the line
	/// could be deleted with the whole suite staying green.
	///
	/// The assertion names the agent's own wording rather than the friendly
	/// prefix: every failure on this path carries the prefix, so asserting it
	/// proves only that something went wrong.
	#[test]
	fn plaintext_base_is_refused_for_being_plaintext() {
		use crate::update::ReleaseSource;
		let src = GitHubSource::with_bases(REPO, "http://127.0.0.1:1", "http://127.0.0.1:1");

		let err = src.latest_version().unwrap_err();
		assert!(
			err.to_string().contains("configured for https only"),
			"the metadata read must be refused on the scheme, got: {err}"
		);

		let err = src.fetch("podup-linux-x86_64").unwrap_err();
		assert!(
			err.to_string().contains("configured for https only"),
			"the download must be refused on the scheme, got: {err}"
		);
	}

	/// The acceptance half of the pair above, one step inside the limit: the
	/// same closed port over https gets past the scheme check and fails on the
	/// socket instead. Without it, a plaintext URL rejected for some unrelated
	/// reason would satisfy the rejection test and prove nothing.
	#[test]
	fn https_base_passes_the_scheme_check() {
		use crate::update::ReleaseSource;
		let src = GitHubSource::with_bases(REPO, "https://127.0.0.1:1", "https://127.0.0.1:1");
		let err = src.latest_version().unwrap_err();
		assert!(
			!err.to_string().contains("configured for https only"),
			"https must not be refused on the scheme, got: {err}"
		);
		assert!(
			err.to_string().contains("Connection refused"),
			"it must fail on the socket instead, got: {err}"
		);
	}
}
