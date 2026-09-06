//! Unit tests for the `to_pretty_json` helper.
//!
//! `to_pretty_json` is the pretty-printed sibling of [`super::to_query_json`],
//! shared by the five `--format json` output sites (`ls`, `images`, `top`,
//! `ps`, `volumes`). The failure contract is the same: a serialisation error
//! must surface as `Err(ComposeError::Build)` naming the field, never as an
//! empty string swallowed by `unwrap_or_default` (#1444).

use super::to_pretty_json;
use crate::error::ComposeError;
use serde::ser::{Error as _, Serializer};
use serde::Serialize;
use serde_json::json;

/// A `Serialize` impl that always errors, so a unit test can pin the
/// "surface the failure" contract without depending on a particular type
/// in the build path. Mirrors the helper used by [`super::to_query_json_tests`].
struct AlwaysFails {
	reason: &'static str,
}

impl Serialize for AlwaysFails {
	fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
		Err(S::Error::custom(self.reason))
	}
}

/// The happy path: a `Vec<serde_json::Value>` of the same shape the five
/// `--format json` call sites serialise round-trips through `to_pretty_json`
/// unchanged. Parse-and-compare rather than pinning the literal pretty
/// format; the round-trip is what matters and is portable across minor
/// formatting tweaks.
#[test]
fn to_pretty_json_serialises_vec_of_json_values() {
	let arr = vec![
		json!({ "Name": "web", "Status": "running(1)" }),
		json!({ "Name": "db", "Status": "exited(1)" }),
	];
	let s = to_pretty_json("ls.row", &arr).unwrap();
	let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
	assert_eq!(
		parsed,
		json!([
			{ "Name": "web", "Status": "running(1)" },
			{ "Name": "db", "Status": "exited(1)" },
		])
	);
}

/// A serialisation failure must surface as `Err(ComposeError::Build)`
/// whose message names the field and the underlying reason. Each of the
/// five `--format json` call sites passes a distinct `what`, so all five
/// labels are pinned to a test.
#[test]
fn to_pretty_json_failure_names_ls_row() {
	let err = to_pretty_json("ls.row", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(matches!(err, ComposeError::Build(_)), "got {err:?}");
	let msg = err.to_string();
	assert!(msg.contains("ls.row"), "got {msg:?}");
	assert!(msg.contains("boom"), "got {msg:?}");
}

#[test]
fn to_pretty_json_failure_names_images_row() {
	let err = to_pretty_json("images.row", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("images.row"), "got {err}");
}

#[test]
fn to_pretty_json_failure_names_top_row() {
	let err = to_pretty_json("top.row", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("top.row"), "got {err}");
}

#[test]
fn to_pretty_json_failure_names_ps_row() {
	let err = to_pretty_json("ps.row", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("ps.row"), "got {err}");
}

#[test]
fn to_pretty_json_failure_names_volumes_row() {
	let err = to_pretty_json("volumes.row", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("volumes.row"), "got {err}");
}
