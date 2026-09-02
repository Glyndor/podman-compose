use super::RELEASE_PUBKEYS;
use base64::Engine as _;
use std::path::Path;

/// Every file in the repository that embeds the key, and the shape it
/// takes there. A new one is a line here.
const COPIES: [&str; 3] = ["install.sh", "install.ps1", "docs/self-update.md"];

/// The unpadded base64 of the key the binary trusts first.
fn binary_key() -> String {
	base64::engine::general_purpose::STANDARD_NO_PAD.encode(RELEASE_PUBKEYS[0])
}

#[test]
fn every_embedded_copy_matches_the_key_the_binary_uses() {
	let root = Path::new(env!("CARGO_MANIFEST_DIR"));
	let want = binary_key();

	for name in COPIES {
		let body = std::fs::read_to_string(root.join(name))
			.unwrap_or_else(|e| panic!("{name} is readable: {e}"));
		assert!(
			body.contains(&want),
			"{name} does not carry the key this binary verifies with ({want}). \
			 A rotation that updates the constant and misses a file fails on a \
			 user's machine, not here."
		);
	}
}

/// The check above only means something if the key is a specific string.
/// Were `binary_key` to return something every file trivially contains,
/// it would pass while proving nothing.
#[test]
fn the_key_is_a_full_length_ed25519_public_key() {
	let key = binary_key();
	assert_eq!(
		key.len(),
		43,
		"unpadded base64 of 32 bytes is 43 chars: {key}"
	);
	assert!(!key.contains('='), "the copies are stored unpadded: {key}");
}
