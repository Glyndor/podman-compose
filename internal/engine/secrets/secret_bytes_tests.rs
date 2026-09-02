use super::*;

/// The redaction is the whole point of [`SecretBytes`], so it is asserted
/// rather than assumed. Without this, someone deriving `Debug` on the type —
/// the obvious thing to reach for — would silently undo it.
#[test]
fn secret_bytes_debug_redacts_the_contents_and_keeps_the_length() {
	let payload = SecretBytes::new(b"a-secret-that-must-not-be-printed".to_vec());
	let shown = format!("{payload:?}");
	assert!(
		!shown.contains("a-secret-that-must-not-be-printed"),
		"the Debug of SecretBytes printed the secret: {shown}"
	);
	// Absence of the secret is not enough on its own: a `Debug` that wrote
	// nothing at all would satisfy it while making every error that mentions a
	// payload useless. The length is the diagnostic the redaction keeps.
	assert_eq!(shown, "<33 bytes of secret redacted>");
}

/// The leak this type replaces is not only the readable one. `Debug` on a
/// bare `&[u8]` prints decimal, so a guard that merely looked for the secret
/// as text would pass over a real disclosure. Recording that here is what
/// keeps the reason for the newtype from being re-litigated as paranoia.
#[test]
fn the_bare_byte_form_this_replaces_leaks_in_decimal() {
	let payload = SecretBytes::new(b"secret".to_vec());
	let bare = format!("{:?}", payload.expose_secret());
	assert_eq!(bare, "[115, 101, 99, 114, 101, 116]");
	assert!(!bare.contains("secret"), "the decimal form is the point");
}

/// [`SecretBytes::expose_secret`] is the only way the bytes get out, so
/// grepping for it is the audit — and an audit surface nothing measures is one
/// that grows. Test code may call it freely; production may not.
#[test]
fn the_escape_hatch_has_exactly_one_production_caller() {
	let sources = [
		("secret_bytes.rs", include_str!("secret_bytes.rs")),
		("plan.rs", include_str!("plan.rs")),
		("create.rs", include_str!("create.rs")),
		("mod.rs", include_str!("mod.rs")),
	];
	let mut callers = Vec::new();
	for (name, src) in sources {
		// Everything from the first `#[cfg(test)]` on is test code.
		let production = src.split("#[cfg(test)]").next().unwrap();
		for line in production.lines() {
			if line.contains("expose_secret()") && !line.contains("fn expose_secret") {
				callers.push(format!("{name}: {}", line.trim()));
			}
		}
	}
	assert_eq!(
		callers.len(),
		1,
		"the secret bytes may leave SecretBytes in exactly one place: the body \
		 of the secrets/create request. Found {callers:#?}"
	);
	assert!(
		callers[0].contains("copy_from_slice"),
		"the one caller moved: {}",
		callers[0]
	);
}
