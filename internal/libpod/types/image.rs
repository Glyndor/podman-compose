//! Podman libpod image API request and response types.

use serde::Deserialize;

/// Streaming JSON line emitted during image pull (`POST /libpod/images/pull`).
#[derive(Deserialize, Default)]
pub struct ImagePullProgress {
	/// Progress text for this line. Mutually exclusive with `error`: on a normal
	/// line this is populated and `error` is empty.
	#[serde(default)]
	pub stream: String,

	/// Error message for this line. Mutually exclusive with `stream`: when the
	/// pull fails this is populated and `stream` is empty.
	#[serde(default)]
	pub error: String,
}

/// Streaming JSON line emitted during image build (`POST /libpod/build`).
#[derive(Deserialize, Default)]
pub struct BuildOutput {
	/// Build log text for this line. Populated on normal output lines; mutually
	/// exclusive with `error`, which is set instead when the build fails.
	#[serde(default)]
	pub stream: String,

	/// Error message for this line; present only when the build fails.
	pub error: Option<String>,

	/// Structured error detail accompanying `error`, when the daemon provides it.
	pub error_detail: Option<BuildErrorDetail>,
}

/// Error detail sub-object in build output.
#[derive(Deserialize)]
pub struct BuildErrorDetail {
	/// Human-readable error message.
	pub message: Option<String>,
}

/// Response from `GET /libpod/images/{name}/json`.
#[derive(Deserialize, Default)]
pub struct ImageInspect {
	/// Image ID (`sha256:...` content digest of the image config).
	#[serde(rename = "Id", default)]
	pub id: String,
	/// Registry digest references (`repo@sha256:...`) for the image, when it was
	/// pulled from (or pushed to) a registry. Used by `config
	/// --resolve-image-digests`. Empty for purely local/built images.
	#[serde(rename = "RepoDigests", default)]
	pub repo_digests: Vec<String>,
	/// On-disk size of the image in bytes, as libpod reports it.
	///
	/// Present in the default inspect response, so reading it costs no extra
	/// call and no query parameter — unlike a container's size, which libpod
	/// leaves `null` until asked. Defaults to zero when the field is absent, so
	/// an older server renders an empty cell rather than failing the whole
	/// listing.
	#[serde(rename = "Size", default)]
	pub size: u64,
	/// When the image was built, as an RFC 3339 string.
	///
	/// Note the shape: the image **list** endpoint reports this as Unix seconds,
	/// the **inspect** endpoint this code calls reports it as RFC 3339. Measured
	/// on Podman 5.7.0; reading the list's documentation and applying it here
	/// yields a parse failure and a blank column.
	#[serde(rename = "Created", default)]
	pub created: String,
}
