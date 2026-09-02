use super::parse;

fn map(content: &str) -> std::collections::HashMap<String, String> {
	parse(content).into_iter().collect()
}

#[test]
fn plain_key_value() {
	let m = map("FOO=bar\nBAZ=qux\n");
	assert_eq!(m["FOO"], "bar");
	assert_eq!(m["BAZ"], "qux");
}

#[test]
fn skips_blank_and_comment_lines() {
	let m = map("# header\n\nFOO=bar\n   # indented comment\n");
	assert_eq!(m.len(), 1);
	assert_eq!(m["FOO"], "bar");
}

#[test]
fn strips_double_quotes() {
	assert_eq!(map("FOO=\"bar\"\n")["FOO"], "bar");
}

#[test]
fn strips_single_quotes() {
	assert_eq!(map("FOO='bar'\n")["FOO"], "bar");
}

#[test]
fn double_quoted_keeps_inner_hash() {
	assert_eq!(map("FOO=\"a # b\"\n")["FOO"], "a # b");
}

#[test]
fn single_quoted_is_literal() {
	assert_eq!(map("FOO='a\\nb'\n")["FOO"], "a\\nb");
}

#[test]
fn double_quoted_expands_escapes() {
	assert_eq!(map("FOO=\"a\\nb\\tc\"\n")["FOO"], "a\nb\tc");
}

#[test]
fn unquoted_strips_inline_comment() {
	assert_eq!(map("FOO=bar # trailing\n")["FOO"], "bar");
}

#[test]
fn unquoted_keeps_leading_hash_without_space() {
	assert_eq!(map("FOO=#notacomment\n")["FOO"], "#notacomment");
}

#[test]
fn unquoted_keeps_internal_spaces() {
	assert_eq!(map("FOO=a b c\n")["FOO"], "a b c");
}

#[test]
fn double_quoted_expands_all_escape_kinds() {
	// `\r`, `\\`, `\"`, `\'`, and an unknown escape (`\z` → `z`) all resolve.
	assert_eq!(map("FOO=\"a\\rb\"\n")["FOO"], "a\rb");
	assert_eq!(map("FOO=\"x\\\\y\"\n")["FOO"], "x\\y");
	assert_eq!(map("FOO=\"q\\\"q\"\n")["FOO"], "q\"q");
	assert_eq!(map("FOO=\"p\\'p\"\n")["FOO"], "p'p");
	assert_eq!(map("FOO=\"a\\zb\"\n")["FOO"], "azb");
}

#[test]
fn export_prefix_bare_key_passes_through_host() {
	// `export NAME` with no `=` passes NAME through from the host environment.
	std::env::set_var("PODUP_DOTENV_EXPORT_BARE", "fromhost");
	let m = map("export PODUP_DOTENV_EXPORT_BARE\n");
	assert_eq!(
		m.get("PODUP_DOTENV_EXPORT_BARE").map(String::as_str),
		Some("fromhost")
	);
	std::env::remove_var("PODUP_DOTENV_EXPORT_BARE");
}

#[test]
fn double_quoted_value_spanning_multiple_lines() {
	// A double-quoted value left open on its line continues until the closing
	// quote on a later line; the newline is preserved.
	let m = map("FOO=\"line one\nline two\"\nBAR=after\n");
	assert_eq!(m["FOO"], "line one\nline two");
	assert_eq!(m["BAR"], "after");
}

#[test]
fn export_prefix_stripped() {
	assert_eq!(map("export FOO=bar\n")["FOO"], "bar");
}

#[test]
fn bare_key_passes_through_or_is_omitted() {
	// Present in the host → passed through; absent → omitted (not empty string).
	std::env::set_var("PODUP_DOTENV_BARE_PRESENT", "v");
	std::env::remove_var("PODUP_DOTENV_BARE_ABSENT");
	let m = map("PODUP_DOTENV_BARE_PRESENT\nPODUP_DOTENV_BARE_ABSENT\n");
	assert_eq!(
		m.get("PODUP_DOTENV_BARE_PRESENT").map(String::as_str),
		Some("v")
	);
	assert!(!m.contains_key("PODUP_DOTENV_BARE_ABSENT"));
	std::env::remove_var("PODUP_DOTENV_BARE_PRESENT");
}

#[test]
fn multiline_double_quoted_value() {
	let m = map("FOO=\"line1\nline2\"\nBAR=baz\n");
	assert_eq!(m["FOO"], "line1\nline2");
	assert_eq!(m["BAR"], "baz");
}

#[test]
fn multiline_single_quoted_value() {
	let m = map("FOO='line1\nline2'\n");
	assert_eq!(m["FOO"], "line1\nline2");
}

#[test]
fn strips_leading_utf8_bom() {
	// A file saved as UTF-8-with-BOM must not capture the first key as
	// `\u{feff}FOO`; the BOM is stripped so FOO resolves normally.
	let m = map("\u{feff}FOO=bar\nBAZ=qux\n");
	assert_eq!(m.get("FOO").map(String::as_str), Some("bar"));
	assert_eq!(m.get("BAZ").map(String::as_str), Some("qux"));
	assert!(!m.keys().any(|k| k.starts_with('\u{feff}')));
}

#[test]
fn parse_strict_strips_leading_bom() {
	let pairs = super::parse_strict("\u{feff}FOO=bar\n").unwrap();
	assert_eq!(pairs, vec![("FOO".to_string(), "bar".to_string())]);
}

#[test]
fn parse_strict_errors_on_unterminated_quote() {
	// An unterminated quote would otherwise absorb every following key into
	// one value, silently dropping them. Strict parsing rejects it.
	let err = super::parse_strict("A=\"oops\nB=keep\n").unwrap_err();
	let msg = err.to_string();
	assert!(msg.contains("unterminated"), "got: {msg}");
	assert!(msg.contains('A'), "should name the offending key: {msg}");
}

#[test]
fn parse_strict_ok_on_terminated_multiline() {
	// A properly closed multi-line value still parses in strict mode and the
	// following key survives.
	let pairs = super::parse_strict("FOO=\"line one\nline two\"\nBAR=after\n").unwrap();
	let m: std::collections::HashMap<_, _> = pairs.into_iter().collect();
	assert_eq!(m["FOO"], "line one\nline two");
	assert_eq!(m["BAR"], "after");
}

#[test]
fn lenient_parse_does_not_error_on_unterminated_quote() {
	// The lenient `.env` path never errors: it degrades to consuming the rest
	// of the file (historical behaviour) rather than failing.
	let pairs = parse("A=\"oops\nB=keep\n");
	assert_eq!(pairs.len(), 1);
	assert_eq!(pairs[0].0, "A");
}

#[test]
fn later_duplicate_returned_after_earlier() {
	let pairs = parse("FOO=first\nFOO=second\n");
	assert_eq!(
		pairs,
		vec![
			("FOO".to_string(), "first".to_string()),
			("FOO".to_string(), "second".to_string()),
		]
	);
}
