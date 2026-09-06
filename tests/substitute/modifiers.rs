use podup::substitute::substitute;
use podup::ComposeError;
use std::collections::HashMap;

fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
	pairs
		.iter()
		.map(|(k, v)| (k.to_string(), v.to_string()))
		.collect()
}

#[test]
fn bare_dollar_sign() {
	assert_eq!(substitute("cost: $5", &vars(&[])).unwrap(), "cost: $5");
}

#[test]
fn double_dollar_escape() {
	assert_eq!(
		substitute("$$VAR", &vars(&[("VAR", "hello")])).unwrap(),
		"$VAR"
	);
}

#[test]
fn bare_var_set() {
	assert_eq!(
		substitute("$FOO bar", &vars(&[("FOO", "hello")])).unwrap(),
		"hello bar"
	);
}

#[test]
fn bare_var_unset() {
	assert_eq!(substitute("$MISSING", &vars(&[])).unwrap(), "");
}

#[test]
fn braced_var_set() {
	assert_eq!(
		substitute("${FOO}", &vars(&[("FOO", "world")])).unwrap(),
		"world"
	);
}

#[test]
fn braced_var_unset() {
	assert_eq!(substitute("${MISSING}", &vars(&[])).unwrap(), "");
}

#[test]
fn default_if_unset_or_empty_nonempty() {
	assert_eq!(
		substitute("${FOO:-def}", &vars(&[("FOO", "bar")])).unwrap(),
		"bar"
	);
}

#[test]
fn default_if_unset_or_empty_empty() {
	assert_eq!(
		substitute("${FOO:-def}", &vars(&[("FOO", "")])).unwrap(),
		"def"
	);
}

#[test]
fn default_if_unset_or_empty_unset() {
	assert_eq!(substitute("${FOO:-def}", &vars(&[])).unwrap(), "def");
}

#[test]
fn default_if_unset_set_empty() {
	assert_eq!(substitute("${FOO-def}", &vars(&[("FOO", "")])).unwrap(), "");
}

#[test]
fn default_if_unset_unset() {
	assert_eq!(substitute("${FOO-def}", &vars(&[])).unwrap(), "def");
}

#[test]
fn default_if_unset_set_nonempty() {
	assert_eq!(
		substitute("${FOO-def}", &vars(&[("FOO", "bar")])).unwrap(),
		"bar"
	);
}

#[test]
fn alt_if_set_and_nonempty() {
	assert_eq!(
		substitute("${FOO:+alt}", &vars(&[("FOO", "bar")])).unwrap(),
		"alt"
	);
}

#[test]
fn alt_if_set_empty_value() {
	assert_eq!(
		substitute("${FOO:+alt}", &vars(&[("FOO", "")])).unwrap(),
		""
	);
}

#[test]
fn alt_if_set_unset() {
	assert_eq!(substitute("${FOO:+alt}", &vars(&[])).unwrap(), "");
}

#[test]
fn alt_if_set_counts_empty() {
	assert_eq!(
		substitute("${FOO+alt}", &vars(&[("FOO", "")])).unwrap(),
		"alt"
	);
}

#[test]
fn alt_if_set_unset_returns_empty() {
	assert_eq!(substitute("${FOO+alt}", &vars(&[])).unwrap(), "");
}

#[test]
fn error_if_unset_or_empty_nonempty() {
	assert_eq!(
		substitute("${FOO:?err}", &vars(&[("FOO", "bar")])).unwrap(),
		"bar"
	);
}

#[test]
fn error_if_unset_or_empty_unset() {
	let result = substitute("${FOO:?err msg}", &vars(&[]));
	assert!(
		matches!(result, Err(ComposeError::RequiredVarNotSet { ref var, ref msg }) if var == "FOO" && msg == "err msg")
	);
}

#[test]
fn error_if_unset_or_empty_empty() {
	assert!(substitute("${FOO:?err msg}", &vars(&[("FOO", "")])).is_err());
}

#[test]
fn error_if_unset_set_empty_ok() {
	assert_eq!(substitute("${FOO?err}", &vars(&[("FOO", "")])).unwrap(), "");
}

#[test]
fn error_if_unset_unset() {
	assert!(substitute("${FOO?err}", &vars(&[])).is_err());
}

#[test]
fn chained() {
	let v = vars(&[("A", "hello"), ("B", "world")]);
	assert_eq!(substitute("$A ${B}", &v).unwrap(), "hello world");
}

#[test]
fn yaml_default_in_string() {
	assert_eq!(
		substitute("image: myapp:${TAG:-latest}", &vars(&[])).unwrap(),
		"image: myapp:latest"
	);
}

// ---------------------------------------------------------------------------
// New: substitution in compose-style positions
// ---------------------------------------------------------------------------

#[test]
fn substitution_in_image_name() {
	let v = vars(&[("VERSION", "1.2.3")]);
	assert_eq!(
		substitute("image: myapp:${VERSION:-dev}", &v).unwrap(),
		"image: myapp:1.2.3"
	);
}

#[test]
fn substitution_in_volume_path() {
	let v = vars(&[]);
	assert_eq!(
		substitute("- ${DATA_DIR:-./data}:/app/data", &v).unwrap(),
		"- ./data:/app/data"
	);
}

#[test]
fn substitution_in_port() {
	let v = vars(&[]);
	assert_eq!(
		substitute("- \"${HOST_PORT:-8080}:80\"", &v).unwrap(),
		"- \"8080:80\""
	);
}

#[test]
fn empty_vs_unset_for_colon_dash() {
	// :- treats both unset and empty as "use default"
	assert_eq!(substitute("${V:-def}", &vars(&[])).unwrap(), "def");
	assert_eq!(substitute("${V:-def}", &vars(&[("V", "")])).unwrap(), "def");
}

#[test]
fn empty_vs_unset_for_dash() {
	// - treats empty as "use the empty string", different from :-
	assert_eq!(substitute("${V-def}", &vars(&[])).unwrap(), "def");
	assert_eq!(substitute("${V-def}", &vars(&[("V", "")])).unwrap(), "");
}

#[test]
fn special_chars_in_default_with_spaces() {
	assert_eq!(
		substitute("${V:-hello world}", &vars(&[])).unwrap(),
		"hello world"
	);
}

#[test]
fn special_chars_in_default_with_url() {
	assert_eq!(
		substitute("${V:-http://example.com}", &vars(&[])).unwrap(),
		"http://example.com"
	);
}

#[test]
fn nested_like_two_subs_in_one_string() {
	let v = vars(&[("V1", "hello"), ("V2", "world")]);
	assert_eq!(substitute("${V1}_${V2}", &v).unwrap(), "hello_world");
}

#[test]
fn trailing_dollar_preserved() {
	assert_eq!(substitute("price: $", &vars(&[])).unwrap(), "price: $");
}
