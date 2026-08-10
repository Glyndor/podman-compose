//! The precomputed libpod `filters` JSON for the per-engine project label.
//!
//! Pulled out of `engine::mod` so the engine module stays under the 500-line
//! hard cap enforced by the org's `line-limit` reusable. The cache is
//! consulted by every container-list / network-list call site that scopes
//! by `podup.project={name}`, plus the dynamic sites (e.g. the
//! `podup.service={svc}` joins) that splice the raw label into a larger
//! filter object (#1364).

use crate::libpod::urlencoded;

/// The pre-URL-encoded libpod `filters` JSON object that scopes a
/// container-list call to this project's `podup.project={name}` label, plus
/// the raw `podup.project={name}` label string. Built once per [`Engine`] so
/// call sites do not pay `format!` + `serde_json::to_string` + `urlencoded`
/// on every invocation (#1364).
///
/// Both halves are returned together because they always come from the same
/// `project` string; the dynamic sites (those that add a second predicate
/// like `podup.service={svc}`) need the unencoded label to splice into their
/// own JSON.
pub(crate) struct ProjectLabelParts {
	/// The URL-encoded `{"label":["podup.project={name}"]}` filter for
	/// container-list calls.
	pub(crate) encoded: String,
	/// The URL-encoded `{"label":["podup.project={name}"]}` filter for
	/// network-list calls. libpod's network and container label filters take
	/// the same shape, so the JSON is the same and only the URL is needed
	/// once (#1364).
	pub(crate) network_encoded: String,
	/// The raw `podup.project={name}` label, for splicing into larger filter
	/// objects.
	pub(crate) raw: String,
}

pub(crate) fn build_project_label_parts(project: &str) -> ProjectLabelParts {
	let raw = format!("podup.project={project}");
	let filter = serde_json::json!({ "label": [raw.clone()] });
	let serialized = filter.to_string();
	ProjectLabelParts {
		encoded: urlencoded(&serialized),
		network_encoded: urlencoded(&serialized),
		raw,
	}
}
