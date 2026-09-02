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
