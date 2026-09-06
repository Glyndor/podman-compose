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
/// it, not of anything stopping them.
///
/// So the guarantee is moved into the type. `SecretBytes` implements neither
/// `Display` nor a derived `Debug`: `format!("{payload}")` does not compile at
/// all, and `format!("{payload:?}")` prints the length instead of the contents.
/// Those two are how every message in this module is built, which leaves exactly
/// one way for the bytes to get out: [`SecretBytes::expose_secret`], named after
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
#[path = "secret_bytes_tests.rs"]
mod tests;
