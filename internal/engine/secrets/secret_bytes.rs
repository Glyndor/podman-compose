//! The wrapper that keeps a secret's bytes out of every message podup writes.
//!
//! Its own file rather than a corner of [`super::plan`] because the point of the
//! type is that its audit is short: what it allows, and who is allowed to unwrap
//! it, should read in one screen without the compose-to-plan mapping around it.

use std::fmt;

/// The bytes of a secret podup creates, in a wrapper that cannot be printed.
///
/// The leak this prevents is not hypothetical shape: every variant of
/// [`ComposeError`] carries a `String`, so any error built on this path is one
/// `format!` away from putting a secret into a public CI log. Keeping that from
/// happening was, until this type existed, a property of nobody having written
/// it — not of anything stopping them.
///
/// So the guarantee is moved into the type. `SecretBytes` implements neither
/// `Display` nor a derived `Debug`: `format!("{payload}")` does not compile at
/// all, and `format!("{payload:?}")` prints the length instead of the contents.
/// Those two are how every message in this module is built, which leaves exactly
/// one way for the bytes to get out — [`SecretBytes::expose_secret`], named after
/// the `secrecy` crate's convention so that grepping for it enumerates the
/// audit surface. It has one caller: the body of the `secrets/create` request.
pub(super) struct SecretBytes(Vec<u8>);

impl SecretBytes {
	pub(super) fn new(bytes: Vec<u8>) -> Self {
		Self(bytes)
	}

	/// Deliberately not called `len`: `clippy::len_without_is_empty` would then
	/// want an `is_empty` this type has no use for, and "byte length" is the more
	/// honest name for the one measurement callers are allowed to take.
	pub(super) fn byte_len(&self) -> usize {
		self.0.len()
	}

	/// The bytes themselves, for the single place they have to leave: the body of
	/// the `secrets/create` request. Any other caller is a leak, which is the
	/// point of it being greppable rather than implicit.
	pub(super) fn expose_secret(&self) -> &[u8] {
		&self.0
	}
}

impl fmt::Debug for SecretBytes {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "<{} bytes of secret redacted>", self.0.len())
	}
}

#[cfg(test)]
mod tests {
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
}
