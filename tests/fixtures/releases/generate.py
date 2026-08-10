#!/usr/bin/env python3
"""Generate the release-signing fixture under tests/fixtures/releases/.

A test keypair (deterministic seed b"\x07" * 32) signs the full set of dummy
release assets the verifier must check at release time:

  SHA256SUMS, install.sh, podup-linux-x86_64, podup-darwin-arm64,
  podup-windows-x86_64.exe, podup_1.0.0_amd64.deb, podup.cdx.json,
  NOTICES.html.

The fixture install.sh carries THREE rotation slots so the SLOT_RE fix is
exercised end-to-end: the test key lives in slot 3 only, and the fixture
fails to verify if the verifier drops anything past slot 2.

Run from the repo root:
  python3 tests/fixtures/releases/generate.py
"""
import base64
import hashlib
import sys
from pathlib import Path

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization

# Three distinct keypairs so the fixture exercises the multi-slot path:
#   slot 1 and slot 2 hold keypairs that do NOT match the test signatures,
#   slot 3 holds the test key that signs every asset.
SLOT1_SEED = b"\x01" * 32
SLOT2_SEED = b"\x02" * 32
TEST_SEED = b"\x07" * 32


def pubkey_b64(seed: bytes) -> str:
	sk = Ed25519PrivateKey.from_private_bytes(seed)
	raw = sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
	return base64.b64encode(raw).decode().rstrip("=")


def pubkey_b64_padded(seed: bytes) -> str:
	sk = Ed25519PrivateKey.from_private_bytes(seed)
	raw = sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
	return base64.b64encode(raw).decode()


def sign(seed: bytes, data: bytes) -> bytes:
	return Ed25519PrivateKey.from_private_bytes(seed).sign(data)


def main() -> int:
	# __file__ is tests/fixtures/releases/generate.py; walk three levels up to
	# land on the repo root regardless of where the script is invoked from.
	repo = Path(__file__).resolve().parents[3]
	out = repo / "tests" / "fixtures" / "releases"
	out.mkdir(parents=True, exist_ok=True)

	slot1 = pubkey_b64(SLOT1_SEED)
	slot2 = pubkey_b64(SLOT2_SEED)
	test_pk = pubkey_b64(TEST_SEED)
	test_pk_padded = pubkey_b64_padded(TEST_SEED)

	# Dummy assets - real releases carry binaries, but the verifier is
	# content-agnostic: a deterministic payload is enough to make the test
	# catch any accidental edit. Bumping this would invalidate every .sig in
	# the fixture, which is what we want - silent change-itis is exactly
	# what the per-asset .sig guards against.
	assets: dict[str, bytes] = {
		"podup-linux-x86_64": b"fixture podup-linux-x86_64 payload",
		"podup-darwin-arm64": b"fixture podup-darwin-arm64 payload",
		"podup-windows-x86_64.exe": b"fixture podup-windows-x86_64.exe payload",
		"podup_1.0.0_amd64.deb": b"fixture podup_1.0.0_amd64.deb payload",
		"podup.cdx.json": b"fixture podup.cdx.json payload",
		"NOTICES.html": b"fixture NOTICES.html payload",
	}

	# SHA256SUMS lists every other asset. Build it BEFORE writing it, but sign
	# it after writing so the signature covers the byte-for-byte manifest.
	lines: list[str] = []
	for name, body in assets.items():
		digest = hashlib.sha256(body).hexdigest()
		lines.append(f"{digest}  {name}")
	manifest = ("\n".join(lines) + "\n").encode("utf-8")
	assets["SHA256SUMS"] = manifest

	for name, body in assets.items():
		(out / name).write_bytes(body)
		(out / f"{name}.sig").write_bytes(sign(TEST_SEED, body))

	# install.sh is the special case: it's a *fixture* install.sh with three
	# rotation slots, NOT a copy of the real install.sh. The verifier reads
	# the slot defaults from this file, so its contents must match the
	# SLOT_RE pattern. Keeping it self-contained (no copy of the real one)
	# ensures exactly three matches - the head of the real install.sh carries
	# its own two slot lines, and copying it would yield four matches and a
	# noisy "verified against slot 3" output.
	fixture_install = (
		"#!/usr/bin/env bash\n"
		"# Fixture install.sh for the release-signing regression test.\n"
		"# Three rotation slots so the SLOT_RE change in verify-signing-key.py\n"
		"# is exercised end-to-end. Slot 1 and slot 2 hold keypairs that do NOT\n"
		"# match the test signatures; slot 3 holds the test key that signs every\n"
		"# other asset in this directory.\n"
		f'\nPODUP_RELEASE_PUBKEY_B64="${{PODUP_RELEASE_PUBKEY_B64:-{slot1}}}"\n'
		f'PODUP_RELEASE_PUBKEY2_B64="${{PODUP_RELEASE_PUBKEY2_B64:-{slot2}}}"\n'
		f'PODUP_RELEASE_PUBKEY3_B64="${{PODUP_RELEASE_PUBKEY3_B64:-{test_pk}}}"\n'
		"\nPUBKEYS=()\n"
		'[[ -n "$PODUP_RELEASE_PUBKEY_B64"  ]] && PUBKEYS+=("$PODUP_RELEASE_PUBKEY_B64")\n'
		'[[ -n "$PODUP_RELEASE_PUBKEY2_B64" ]] && PUBKEYS+=("$PODUP_RELEASE_PUBKEY2_B64")\n'
		'[[ -n "$PODUP_RELEASE_PUBKEY3_B64" ]] && PUBKEYS+=("$PODUP_RELEASE_PUBKEY3_B64")\n'
	)
	(out / "install.sh").write_text(fixture_install, encoding="utf-8")
	(out / "install.sh.sig").write_bytes(sign(TEST_SEED, fixture_install.encode("utf-8")))

	# Persist the test-key fingerprint so a reviewer can confirm the fixture
	# really uses the seed documented at the top of this file.
	(out / "test-key.b64").write_text(test_pk_padded + "\n", encoding="utf-8")

	print(f"generated fixture under {out}")
	print(f"  slot 1 pubkey (mismatch): {slot1}")
	print(f"  slot 2 pubkey (mismatch): {slot2}")
	print(f"  slot 3 pubkey (matches) : {test_pk}")
	return 0


if __name__ == "__main__":
	sys.exit(main())
