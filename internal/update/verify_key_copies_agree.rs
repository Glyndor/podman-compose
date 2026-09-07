use super::RELEASE_PUBKEYS;
use base64::Engine as _;
use std::path::Path;

/// Every file in the repository that embeds the key, and the shape it
/// takes there. A new one is a line here.
const COPIES: [&str; 3] = ["install.sh", "install.ps1", "docs/self-update.md"];

/// The unpadded-base64 view of every non-empty slot in `keys`, in the same
/// shape the embedded copies are stored as. Slot 0 is the live key; slot 1
/// starts zeroed (the steady-state shape) and is populated during the next
/// rotation. Iterating over the full array (with the empty-slot filter)
/// means a rotation that adds a second live key to the constant also adds
/// it to the check (#1747), so the "every copy carries the key"
/// guarantee that has always held for slot 0 picks up the new slot on
/// the same day.
fn encoded_keys(keys: &[[u8; 32]]) -> Vec<String> {
	keys.iter()
		.filter(|raw| **raw != [0u8; 32])
		.map(|raw| base64::engine::general_purpose::STANDARD_NO_PAD.encode(raw))
		.collect()
}

/// The view `every_embedded_copy_matches_every_configured_release_key`
/// checks against; reads the production constant. Wrapping the constant
/// pass keeps the public test asserting against the live build and lets
/// the unit test exercise the filter directly with a hand-built fixture.
fn binary_keys() -> Vec<String> {
	encoded_keys(&RELEASE_PUBKEYS)
}

#[test]
fn every_embedded_copy_matches_every_configured_release_key() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let wants = binary_keys();
	assert!(
		!wants.is_empty(),
		"the binary must verify against at least one release key"
	);

	for name in COPIES {
		let body = std::fs::read_to_string(root.join(name))
			.unwrap_or_else(|e| panic!("{name} is readable: {e}"));
		for want in &wants {
			assert!(
				body.contains(want),
				"{name} does not carry the key this binary verifies with \
                 ({want}). A rotation that populates a second slot and \
                 misses a file fails on a user's machine, not here."
			);
		}
	}
}

/// The check above only means something if the keys are specific strings.
/// Were `binary_keys` to return something every file trivially contains,
/// it would pass while proving nothing. Every configured key is the
/// unpadded base64 of a 32-byte Ed25519 public key.
#[test]
fn every_configured_key_is_a_full_length_ed25519_public_key() {
	let keys = binary_keys();
	assert!(!keys.is_empty(), "at least one configured release key");
	for key in &keys {
		assert_eq!(
			key.len(),
			43,
			"unpadded base64 of 32 bytes is 43 chars: {key}"
		);
		assert!(!key.contains('='), "the copies are stored unpadded: {key}");
	}
}

/// #1747 (L9): `encoded_keys` used to take `keys[0]` only. Today
/// `RELEASE_PUBKEYS[1]` is `[0u8; 32]` (the empty rotation slot), so
/// both old and new code see only one key and the test passes either
/// way; the difference is the day `RELEASE_PUBKEYS[1]` is populated.
/// This fixture exercises that path by passing two non-empty slots to
/// the helper and checking it returns both. Slot 0 alone is what the
/// pre-fix code returned; slot 1 alone is the rotation-day shape; both
/// together is steady-state mid-rotation.
#[test]
fn encoded_keys_includes_every_non_empty_slot_not_just_the_first() {
	use base64::engine::general_purpose::STANDARD_NO_PAD;
	let a = [42u8; 32];
	let b = [7u8; 32];
	let a_str = STANDARD_NO_PAD.encode(a);
	let b_str = STANDARD_NO_PAD.encode(b);
	// Old code (had it returned just `keys[0]`): `[a_str]`.
	// New code: `[a_str, b_str]`.
	let out = encoded_keys(&[a, b]);
	assert!(
		out.contains(&a_str) && out.contains(&b_str),
		"both slots must be returned; got {out:?}"
	);
	assert_eq!(
		out.len(),
		2,
		"the empty-slot filter must keep both live slots"
	);
}

/// An empty (all-zero) slot is the steady-state shape for `RELEASE_PUBKEYS`
/// before rotation. The filter rejects it so a binary built with the
/// constant in its rotation-day shape still surfaces the configured key.
#[test]
fn encoded_keys_skips_all_zero_slots() {
	let out = encoded_keys(&[[0u8; 32], [0u8; 32]]);
	assert!(out.is_empty(), "no live key, no rows: {out:?}");
}
