//! Verification primitives for self-update: the security core.
//!
//! Trust anchor is the set of Ed25519 public keys embedded in this binary
//! ([`RELEASE_PUBKEYS`]), not the download domain or TLS. A release is accepted
//! only if `SHA256SUMS` carries a valid signature from a matching private key
//! (held as a CI secret) and the downloaded binary's SHA-256 digest appears in
//! that signed manifest. Every check fails closed.

use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::ComposeError;

/// Accepted Ed25519 release public keys, at most two. Slot 0 holds the active
/// release key (`GLYNDOR_RELEASE_ED25519_KEY`); slot 1 is the empty rotation
/// slot, populated only during a key rotation (see below). A signature is
/// trusted if it validates under either non-zero slot. The keys are public by
/// design: their integrity comes from being baked into the signed,
/// build-provenance-attested binary, so an attacker cannot swap them without
/// invalidating the binary itself.
///
/// Verified against the genuine published `SHA256SUMS.sig` (see
/// `embedded_key_verifies_real_release`). [`release_pubkeys`] still fails closed
/// if both are zeroed, so a misbuild can never trust an unverifiable release.
///
/// # Key rotation
///
/// The make-before-break procedure below assumes the outgoing private key is
/// still available to sign the migration release. That is the normal case.
///
/// 1. Ship a release embedding `[old, new]` with `SHA256SUMS` signed by the
///    **old** key. Binaries in the field trust only `old`, so they accept it and
///    upgrade, picking up `new` in the process.
/// 2. Ship the next release embedding `[new, zero]` signed by the **new** key.
///    Every binary from step 1 trusts `new`, so the old key is retired and all
///    installs converge on the new key.
///
/// If the outgoing private key is LOST, step 1 is impossible (no release can be
/// signed by the old key) so fielded self-updaters cannot migrate in-band and
/// must be re-installed out-of-band (rotated `install.sh` / apt). That happened
/// here: the key below is a fresh key with no relationship to any previously
/// embedded key, and slot 1 starts zeroed (the normal steady state) rather than
/// carrying a second live key.
pub const RELEASE_PUBKEYS: [[u8; 32]; 2] = [
	// GLYNDOR_RELEASE_ED25519_KEY = HFv7vg5FCY7YyKUDbJhaQSfB9SboJGSblJtFbLmLHzM
	[
		28, 91, 251, 190, 14, 69, 9, 142, 216, 200, 165, 3, 108, 152, 90, 65, 39, 193, 245, 38,
		232, 36, 100, 155, 148, 155, 69, 108, 185, 139, 31, 51,
	],
	// Empty rotation slot; populate during the next key rotation.
	[0u8; 32],
];

/// A parsed `MAJOR.MINOR.PATCH` version, ordered for comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
	pub major: u64,
	pub minor: u64,
	pub patch: u64,
}

/// Parse a `vX.Y.Z` or `X.Y.Z` version string. Anything else is rejected so a
/// malformed tag can never be mistaken for "newer".
pub fn parse_version(s: &str) -> crate::Result<Version> {
	let trimmed = s.trim();
	let core = trimmed.strip_prefix('v').unwrap_or(trimmed);
	let mut parts = core.split('.');
	let mut next = |what: &str| -> crate::Result<u64> {
		parts
			.next()
			.and_then(|p| p.parse::<u64>().ok())
			.ok_or_else(|| ComposeError::Update(format!("invalid version '{s}': bad {what}")))
	};
	let major = next("major")?;
	let minor = next("minor")?;
	let patch = next("patch")?;
	if parts.next().is_some() {
		return Err(ComposeError::Update(format!(
			"invalid version '{s}': too many components"
		)));
	}
	Ok(Version {
		major,
		minor,
		patch,
	})
}

/// Decode the configured release public keys, skipping empty rotation slots.
/// Fails closed if none remain (verification key not configured for this build)
/// or a configured key is malformed.
pub fn release_pubkeys() -> crate::Result<Vec<VerifyingKey>> {
	let mut keys = Vec::new();
	for raw in &RELEASE_PUBKEYS {
		if raw == &[0u8; 32] {
			continue;
		}
		let key = VerifyingKey::from_bytes(raw)
			.map_err(|e| ComposeError::Update(format!("embedded release key is invalid: {e}")))?;
		keys.push(key);
	}
	if keys.is_empty() {
		return Err(ComposeError::Update(
			"release verification key not configured in this build; refusing to self-update"
				.to_string(),
		));
	}
	Ok(keys)
}

/// Verify that `signature` (raw 64-byte Ed25519) over `message` validates under
/// any of `keys`. Fails closed on a wrong length or a mismatch against every
/// key. Kept separate from [`verify_signature`] so the multi-key logic is
/// testable without touching the embedded constant.
fn verify_with_keys(keys: &[VerifyingKey], message: &[u8], signature: &[u8]) -> crate::Result<()> {
	let sig = Signature::from_slice(signature).map_err(|_| {
		ComposeError::Update(format!(
			"malformed signature: expected 64 bytes, got {}",
			signature.len()
		))
	})?;
	if keys
		.iter()
		.any(|key| key.verify_strict(message, &sig).is_ok())
	{
		Ok(())
	} else {
		Err(ComposeError::Update(
			"signature verification failed; release may be tampered or unsigned".to_string(),
		))
	}
}

/// Verify that `signature` (raw 64-byte Ed25519) over `message` was produced by
/// one of the accepted release keys. Fails closed on a wrong length, no
/// configured key, or a mismatch against every key.
pub fn verify_signature(message: &[u8], signature: &[u8]) -> crate::Result<()> {
	verify_with_keys(&release_pubkeys()?, message, signature)
}

/// Verify `signature` against the embedded key using an explicitly supplied key
/// Test seam so the signature path is exercised without the placeholder guard.
#[cfg(test)]
pub fn verify_signature_with(
	key: &VerifyingKey,
	message: &[u8],
	signature: &[u8],
) -> crate::Result<()> {
	let sig = Signature::from_slice(signature)
		.map_err(|_| ComposeError::Update("malformed signature".to_string()))?;
	key.verify_strict(message, &sig)
		.map_err(|_| ComposeError::Update("signature verification failed".to_string()))
}

/// Look up the expected lowercase-hex SHA-256 digest for `asset` in a signed
/// `SHA256SUMS` manifest (`<hex>␠␠<name>` or `<hex>␠*<name>` lines).
pub fn expected_digest(sha256sums: &[u8], asset: &str) -> crate::Result<String> {
	let text = std::str::from_utf8(sha256sums)
		.map_err(|_| ComposeError::Update("SHA256SUMS is not valid UTF-8".to_string()))?;
	for line in text.lines() {
		let line = line.trim();
		let Some((hex, name)) = line.split_once(char::is_whitespace) else {
			continue;
		};
		// Strip the optional binary-mode '*' marker on the filename.
		let name = name.trim().trim_start_matches('*');
		if name == asset {
			let hex = hex.trim().to_ascii_lowercase();
			if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
				return Err(ComposeError::Update(format!(
					"SHA256SUMS has a malformed digest for {asset}"
				)));
			}
			return Ok(hex);
		}
	}
	Err(ComposeError::Update(format!(
		"{asset} is not listed in SHA256SUMS"
	)))
}

/// Compute the lowercase-hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
	let digest = Sha256::digest(data);
	let mut out = String::with_capacity(64);
	for byte in digest {
		// Each nibble is in 0..=15, always a valid radix-16 digit.
		out.push(char::from_digit((byte >> 4) as u32, 16).expect("high nibble is a hex digit"));
		out.push(char::from_digit((byte & 0xf) as u32, 16).expect("low nibble is a hex digit"));
	}
	out
}

/// Compare two byte slices in constant time, returning `true` when equal.
///
/// The running time depends only on the length, not on where the first
/// differing byte sits, so it leaks no information about a partial match.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
	if a.len() != b.len() {
		return false;
	}
	let mut diff = 0u8;
	for (x, y) in a.iter().zip(b.iter()) {
		diff |= x ^ y;
	}
	diff == 0
}

/// Verify the downloaded bytes hash to `expected_hex` (case-insensitive).
///
/// The digest comparison runs in constant time so it cannot leak how many
/// leading bytes matched.
pub fn verify_digest(data: &[u8], expected_hex: &str) -> crate::Result<()> {
	let actual = sha256_hex(data);
	let expected = expected_hex.to_ascii_lowercase();
	if constant_time_eq(actual.as_bytes(), expected.as_bytes()) {
		Ok(())
	} else {
		Err(ComposeError::Update(format!(
			"checksum mismatch: expected {expected_hex}, got {actual}"
		)))
	}
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;

/// The release key is embedded in four places in this repository, and it has
/// to be: a consumer that downloaded the key it verifies with would be
/// handing the decision to whoever controls the download. Duplication is the
/// correct design here, so the risk is not that copies exist; it is that a
/// rotation updates some and misses others.
///
/// A miss is silent. `install.sh` would hand out a binary the self-updater
/// then refuses, or the docs would tell someone to check a signature against
/// a key nothing signs with any more. Nothing errors at the point of the
/// mistake; it surfaces later, on a user's machine.
///
/// So this compares the text copies against the bytes the binary actually
/// verifies with, rather than against each other. A comment that drifts from
/// the constant beside it is caught too.
#[cfg(test)]
#[path = "verify_key_copies_agree.rs"]
mod key_copies_agree;
