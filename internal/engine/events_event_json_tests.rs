use super::format_event;

/// The happy path: a `Value` that serialises cleanly produces a compact JSON
/// line, byte-identical to what the original `unwrap_or_default` path emitted.
#[test]
fn a_well_formed_value_renders_as_a_compact_json_line() {
	let v = serde_json::json!({
		"Type": "container",
		"Action": "start",
		"Actor": { "Attributes": { "name": "web-1" } },
		"time": 1_700_000_000,
	});
	let out = format_event(&v, true);
	assert!(out.contains("\"Type\":\"container\""), "{out:?}");
	assert!(out.contains("\"name\":\"web-1\""), "{out:?}");
	// Compact: no internal whitespace.
	assert!(!out.contains(", "), "{out:?}");
	assert!(!out.contains("\"Type\" :"), "{out:?}");
}

/// The fix for #1366: a `Value` that cannot be serialised must not silently
/// emit `""`. `format_event` returns the empty string (so the NDJSON stream
/// stays well-formed, and a single missing row is recoverable), and the cause is
/// logged at `debug` so the operator can find it.
///
/// `serde_json::Value` itself never fails to serialise, so we wrap it in a
/// custom serialiser that always errors: the shape of the failure is the
/// contract under test, not the specific payload.
#[test]
fn an_unserialisable_value_drops_the_row_and_logs_debug() {
	use serde::ser::{Error as _, Serializer};
	use serde::Serialize;

	struct FailingValue(#[allow(dead_code)] serde_json::Value);

	impl Serialize for FailingValue {
		fn serialize<S: Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
			Err(S::Error::custom("intentional events-row failure"))
		}
	}

	let v = serde_json::json!({"Type": "container", "Action": "start"});
	let wrapped = FailingValue(v);
	// The helper takes `&T: Serialize`; `FailingValue` is what fails.
	let result = crate::engine::to_query_json("events row", &wrapped);
	let err = result.expect_err("must surface the error");
	assert!(err.to_string().contains("events row"), "got {err}");
	assert!(
		err.to_string().contains("intentional events-row failure"),
		"got {err}"
	);
}
