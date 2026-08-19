//! Podman libpod REST API client and types.
//!
//! Talks to Podman's native libpod API at `/libpod/...` instead of the
//! Docker-compatibility layer. Eliminates the Docker compat intermediary,
//! gives access to Podman-native error messages, and removes the bollard
//! dependency.

pub mod client;
pub mod error;
pub mod types;
pub(crate) mod validate;

/// Path prefix for every libpod REST route.
///
/// Podman's libpod routes are version-namespaced: an unversioned path such as
/// `/libpod/containers/json` is not guaranteed to resolve, so every request
/// must carry the API version. `v5.0.0` is the libpod API version this client
/// targets; podup requires Podman >= 5.0 and supports Podman 5 and 6. Podman
/// keeps the route surface backward-compatible across majors, so a Podman 6
/// server still resolves the `v5.0.0` prefix (validated by the `podman-lane`
/// integration, which runs the suite on both Podman 5 and 6). The
/// version-independent `/libpod/_ping` endpoint is the sole
/// exception and deliberately omits this prefix.
pub(crate) const API_PREFIX: &str = "/v5.0.0/libpod";

pub use client::Client;
pub(crate) use client::{is_valid_object_name, urlencoded};
pub use error::PodmanError;
pub use types::stream::{parse_json_lines, parse_multiplexed, parse_raw, LogOutput};
