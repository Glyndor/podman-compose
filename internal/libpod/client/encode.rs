//! Percent-encoding for libpod REST path segments.

/// Percent-encode a string for use as a single URL path/query segment, encoding
/// everything outside the RFC 3986 unreserved set so container names, project
/// names, and tags can contain arbitrary bytes without breaking the request.
pub(crate) fn urlencoded(s: &str) -> String {
	let mut out = String::with_capacity(s.len());
	for b in s.bytes() {
		match b {
			b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
				out.push(b as char);
			}
			_ => {
				out.push('%');
				out.push(
					char::from_digit((b >> 4) as u32, 16)
						.unwrap()
						.to_ascii_uppercase(),
				);
				out.push(
					char::from_digit((b & 0xf) as u32, 16)
						.unwrap()
						.to_ascii_uppercase(),
				);
			}
		}
	}
	out
}

/// Whether `name` matches podman's object-name pattern
/// (`[a-zA-Z0-9][a-zA-Z0-9_.-]*`): a leading ASCII alphanumeric followed by
/// alphanumerics, `_`, `.`, or `-`. Used to reject an invalid container/network/
/// volume name client-side with a clear message instead of deferring to an
/// opaque podman HTTP 500. Pure so it is unit-tested.
pub(crate) fn is_valid_object_name(name: &str) -> bool {
	let mut chars = name.chars();
	match chars.next() {
		Some(c) if c.is_ascii_alphanumeric() => {}
		_ => return false,
	}
	chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;
