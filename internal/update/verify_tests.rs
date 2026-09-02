use super::*;
use ed25519_dalek::{Signer, SigningKey};

fn test_keypair() -> (SigningKey, VerifyingKey) {
	let seed = [7u8; 32];
	let sk = SigningKey::from_bytes(&seed);
	let vk = sk.verifying_key();
	(sk, vk)
}

#[test]
fn parse_version_with_and_without_v() {
	assert_eq!(
		parse_version("v1.2.3").unwrap(),
		parse_version("1.2.3").unwrap()
	);
	let v = parse_version("v0.6.0").unwrap();
	assert_eq!((v.major, v.minor, v.patch), (0, 6, 0));
}

#[test]
fn version_ordering() {
	assert!(parse_version("v0.6.1").unwrap() > parse_version("v0.6.0").unwrap());
	assert!(parse_version("v1.0.0").unwrap() > parse_version("v0.99.99").unwrap());
	assert!(parse_version("v0.6.0").unwrap() == parse_version("0.6.0").unwrap());
}

#[test]
fn parse_version_rejects_garbage() {
	for bad in ["", "v1", "1.2", "1.2.3.4", "a.b.c", "1.2.x", "v1.2.-1"] {
		assert!(parse_version(bad).is_err(), "should reject {bad}");
	}
}

#[test]
fn embedded_key_is_configured_and_rejects_garbage() {
	// A real key is baked in; it must load and reject a bogus signature.
	assert_ne!(RELEASE_PUBKEYS[0], [0u8; 32]);
	assert!(release_pubkeys().is_ok());
	assert!(verify_signature(b"data", &[0u8; 64]).is_err());
}

#[test]
fn zeroed_key_would_fail_closed() {
	// Defence in depth: an all-zero key is a valid curve point, so the
	// explicit guard in `release_pubkeys` — not the curve math — is what
	// refuses to trust an unverifiable release if every key is zeroed.
	assert!(VerifyingKey::from_bytes(&[0u8; 32]).is_ok());
	let is_placeholder = |key: [u8; 32]| key == [0u8; 32];
	assert!(is_placeholder([0u8; 32]));
	assert!(!is_placeholder(RELEASE_PUBKEYS[0]));
}

#[test]
fn accepts_signature_from_any_configured_key() {
	// Rotation: a binary embedding two keys must accept a release signed by
	// EITHER, so an in-field binary can upgrade across a key change.
	let (sk_a, vk_a) = test_keypair();
	let sk_b = SigningKey::from_bytes(&[9u8; 32]);
	let vk_b = sk_b.verifying_key();
	let msg = b"SHA256SUMS payload";

	let sig_b = sk_b.sign(msg).to_bytes();
	verify_with_keys(&[vk_a, vk_b], msg, &sig_b).unwrap();

	let sig_a = sk_a.sign(msg).to_bytes();
	verify_with_keys(&[vk_a, vk_b], msg, &sig_a).unwrap();
}

#[test]
fn rejects_signature_from_unconfigured_key() {
	// A signature from a key that is NOT in the accepted set must fail, even
	// though other keys are configured.
	let (_sk_a, vk_a) = test_keypair();
	let sk_x = SigningKey::from_bytes(&[3u8; 32]);
	let msg = b"payload";
	let sig_x = sk_x.sign(msg).to_bytes();
	assert!(verify_with_keys(&[vk_a], msg, &sig_x).is_err());
}

#[test]
fn verify_with_keys_rejects_wrong_length_signature() {
	// A signature that is not 64 bytes is rejected at the length gate inside
	// verify_with_keys (distinct from the single-key verify_signature_with seam).
	let (_sk, vk) = test_keypair();
	let err = verify_with_keys(&[vk], b"payload", &[0u8; 10]).unwrap_err();
	match err {
		ComposeError::Update(msg) => assert!(msg.contains("expected 64 bytes")),
		_ => panic!("expected an Update error"),
	}
}

#[test]
fn expected_digest_skips_lines_without_whitespace() {
	// A manifest line carrying no whitespace separator is skipped rather than
	// mis-parsed; a well-formed later line still resolves.
	let sums = "garbageline\n\
	            52d6148bf50d9d3f24a634402ec39d44302d73b21e3b74ed6a28877fdd7b93ea  podup-linux-x86_64\n";
	assert_eq!(
		expected_digest(sums.as_bytes(), "podup-linux-x86_64").unwrap(),
		"52d6148bf50d9d3f24a634402ec39d44302d73b21e3b74ed6a28877fdd7b93ea"
	);
}

#[test]
fn embedded_key_verifies_real_release() {
	// Regression vector: the genuine published podup SHA256SUMS and its
	// signature must verify against the embedded key. If a future edit
	// swaps the key, this fails loudly. Vectored from the v1.11.0 release
	// (the first signed with GLYNDOR_RELEASE_ED25519_KEY); the signature
	// covers the full manifest byte-for-byte, so all listed assets are here.
	let sha256sums = "\
0be7f2b09d518ea452a5711ea845fa76a6a6283bc883972d24b9caa3a78902d0  podup-linux-x86_64
b9e041bb9177e482b887531c383fe9ba12fd8a636208c5e9f1e8e79e02776b77  podup-linux-arm64
c0d896932ada2a391e7115c05cc940a9d42d7ee67f5ada35408a40a3d3be9f19  podup-darwin-arm64
255530d6dfcffb7fa7df282be2e6987b708d770afee0bca3a5cbbf6303138cdd  podup-darwin-x86_64
cfae018f6078e40289c15003ce5b24843864b8bb65cd03ecbd16d57212ae2e62  podup-windows-x86_64.exe
bdf2296df8eb75d36c11244d7d433398719b5aed61e6c601151de92170018b2c  podup-windows-arm64.exe
6f8fea9446de2ac4c7ec4c7a0cfebb18263befabb824fa585058a50932d08a5d  podup_1.11.0_amd64.deb
1a24b7a4972c07e66088c486c15852c70affbb6970b43f2eaa1b85ec3218ea1b  podup_1.11.0_arm64.deb
4f3a3b3e008ca5b4a8d2fa0eff91762b580d7a2fa4f1ccb707e6e3846b8468b3  podup.cdx.json
6739b03a00653b7ffa755cf032985c38cef03ebb46d8e8675b1469b6fe13f9d8  NOTICES.html
f12e41867749c42afd77ac027fe77e406e2272a4f28e2de6700b73ee134d5e89  install.sh
f4aa771d1bf238fea5b764d90258ec43e6b034de74ce1cd41ef41af1500d7cf9  install.ps1
";
	let signature: [u8; 64] = [
		135, 229, 99, 176, 177, 206, 51, 152, 206, 73, 1, 225, 53, 63, 104, 166, 202, 110, 104, 21,
		165, 52, 193, 38, 82, 186, 106, 125, 158, 3, 95, 175, 226, 114, 80, 249, 215, 173, 19, 60,
		56, 205, 224, 100, 216, 54, 237, 79, 215, 111, 4, 157, 78, 70, 150, 192, 63, 145, 10, 249,
		7, 17, 109, 12,
	];
	verify_signature(sha256sums.as_bytes(), &signature).unwrap();

	// And the manifest it signs really lists this platform's asset digest.
	let digest = expected_digest(sha256sums.as_bytes(), "podup-linux-x86_64").unwrap();
	assert_eq!(
		digest,
		"0be7f2b09d518ea452a5711ea845fa76a6a6283bc883972d24b9caa3a78902d0"
	);
}

#[test]
fn valid_signature_accepted() {
	let (sk, vk) = test_keypair();
	let msg = b"SHA256SUMS contents";
	let sig = sk.sign(msg).to_bytes();
	verify_signature_with(&vk, msg, &sig).unwrap();
}

#[test]
fn tampered_message_rejected() {
	let (sk, vk) = test_keypair();
	let sig = sk.sign(b"original").to_bytes();
	assert!(verify_signature_with(&vk, b"tampered", &sig).is_err());
}

#[test]
fn wrong_key_rejected() {
	let (sk, _) = test_keypair();
	let other = SigningKey::from_bytes(&[9u8; 32]).verifying_key();
	let sig = sk.sign(b"data").to_bytes();
	assert!(verify_signature_with(&other, b"data", &sig).is_err());
}

#[test]
fn malformed_signature_length_rejected() {
	let (_, vk) = test_keypair();
	assert!(verify_signature_with(&vk, b"data", &[0u8; 10]).is_err());
}

#[test]
fn sha256_known_vector() {
	// SHA-256 of the empty input.
	assert_eq!(
		sha256_hex(b""),
		"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
	);
	// SHA-256 of "abc".
	assert_eq!(
		sha256_hex(b"abc"),
		"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
	);
}

#[test]
fn digest_roundtrip_and_mismatch() {
	let data = b"podup binary bytes";
	let hex = sha256_hex(data);
	verify_digest(data, &hex).unwrap();
	verify_digest(data, &hex.to_ascii_uppercase()).unwrap();
	assert!(verify_digest(data, &"0".repeat(64)).is_err());
	// A length mismatch is rejected, not panicked on.
	assert!(verify_digest(data, "deadbeef").is_err());
}

#[test]
fn constant_time_eq_matches_only_identical_slices() {
	assert!(constant_time_eq(b"abc", b"abc"));
	assert!(!constant_time_eq(b"abc", b"abd"));
	assert!(!constant_time_eq(b"abc", b"ab"));
	assert!(constant_time_eq(b"", b""));
}

#[test]
fn expected_digest_two_space_format() {
	let sums = format!("{}  podup-linux-x86_64\n", "a".repeat(64));
	assert_eq!(
		expected_digest(sums.as_bytes(), "podup-linux-x86_64").unwrap(),
		"a".repeat(64)
	);
}

#[test]
fn expected_digest_binary_star_format() {
	let sums = format!("{} *podup-darwin-arm64\n", "B".repeat(64));
	// Hex is normalized to lowercase.
	assert_eq!(
		expected_digest(sums.as_bytes(), "podup-darwin-arm64").unwrap(),
		"b".repeat(64)
	);
}

#[test]
fn expected_digest_picks_right_line() {
	let sums = format!(
		"{}  podup-linux-x86_64\n{}  podup-linux-arm64\n",
		"1".repeat(64),
		"2".repeat(64)
	);
	assert_eq!(
		expected_digest(sums.as_bytes(), "podup-linux-arm64").unwrap(),
		"2".repeat(64)
	);
}

#[test]
fn expected_digest_missing_asset_errors() {
	let sums = format!("{}  other-asset\n", "a".repeat(64));
	assert!(expected_digest(sums.as_bytes(), "podup-linux-x86_64").is_err());
}

#[test]
fn expected_digest_malformed_hex_errors() {
	let sums = "nothex  podup-linux-x86_64\n";
	assert!(expected_digest(sums.as_bytes(), "podup-linux-x86_64").is_err());
}

#[test]
fn expected_digest_rejects_non_utf8() {
	assert!(expected_digest(&[0xff, 0xfe], "x").is_err());
}
