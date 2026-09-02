use super::to_query_json;
use crate::error::ComposeError;
use serde::ser::{Error as _, Serializer};
use serde::Serialize;
use std::collections::HashMap;

/// A `Serialize` impl that always errors, so a unit test can pin the
/// "surface the failure" contract without depending on a particular type
/// in the build path.
struct AlwaysFails {
	reason: &'static str,
}

impl Serialize for AlwaysFails {
	fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
		Err(S::Error::custom(self.reason))
	}
}

/// The happy path: a `Vec<String>` round-trips through `to_query_json`
/// unchanged. This is the same shape the `cachefrom` and `cacheto` sites
/// serialise.
#[test]
fn to_query_json_serialises_vec_string() {
	let v: Vec<String> = vec!["alpine".into(), "quay.io/lib/alpine".into()];
	assert_eq!(
		to_query_json("build.cache_from", &v).unwrap(),
		r#"["alpine","quay.io/lib/alpine"]"#
	);
}

/// The happy path for the `buildargs` and `labels` sites: a `HashMap<String,
/// String>` serialises to a JSON object.
#[test]
fn to_query_json_serialises_hashmap_string_string() {
	let mut m: HashMap<String, String> = HashMap::new();
	m.insert("VERSION".into(), "1.2.3".into());
	let s = to_query_json("build.args", &m).unwrap();
	// Object order isn't stable across HashMap iterations, so parse and
	// check rather than comparing the literal.
	let parsed: serde_json::Value = serde_json::from_str(&s).unwrap();
	assert_eq!(parsed["VERSION"], "1.2.3");
}

/// The happy path for the `secrets` site: a `Vec<String>` of `id=…,src=…`
/// specs.
#[test]
fn to_query_json_serialises_secrets_vec() {
	let v: Vec<String> = vec!["id=tok,src=.podup-build-secret-tok".into()];
	assert_eq!(
		to_query_json("build.secrets", &v).unwrap(),
		r#"["id=tok,src=.podup-build-secret-tok"]"#
	);
}

/// A serialisation failure must surface as `Err(ComposeError::Build)`
/// whose message names the field and the underlying reason. Each of the
/// five build sites passes a distinct `what`, so all five labels are
/// pinned to a test.
#[test]
fn to_query_json_failure_names_build_cache_from() {
	let err = to_query_json("build.cache_from", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(matches!(err, ComposeError::Build(_)), "got {err:?}");
	let msg = err.to_string();
	assert!(msg.contains("build.cache_from"), "got {msg:?}");
	assert!(msg.contains("boom"), "got {msg:?}");
}

#[test]
fn to_query_json_failure_names_build_args() {
	let err = to_query_json("build.args", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("build.args"), "got {err}");
}

#[test]
fn to_query_json_failure_names_build_labels() {
	let err = to_query_json("build.labels", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("build.labels"), "got {err}");
}

#[test]
fn to_query_json_failure_names_build_secrets() {
	let err = to_query_json("build.secrets", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("build.secrets"), "got {err}");
}

#[test]
fn to_query_json_failure_names_build_cache_to() {
	let err = to_query_json("build.cache_to", &AlwaysFails { reason: "boom" })
		.expect_err("must surface the error");
	assert!(err.to_string().contains("build.cache_to"), "got {err}");
}
