#!/usr/bin/env python3
"""Sign a file with the active Glyndor release Ed25519 key (32-byte seed, base64).

Usage:
  sign.py <input-file> [<output-sig-file>]

The active signing seed is read from GLYNDOR_RELEASE_ED25519_KEY, in the
environment and never on the command line, so it never appears in the process
argument list (/proc/<pid>/cmdline). It is a raw 32-byte Ed25519 private key in
standard base64. When output-sig-file is omitted, writes <input-file>.sig.

Rotation is by dual-TRUST, not dual-sign: consumers (install.sh, install.ps1,
verify-debs.sh, internal/update/verify.rs) bake BOTH accepted public keys and
accept a release signed by either, so the active signer can be switched to the
second key with no consumer change. Only one detached .sig is ever produced, the
one signature every verifier reads. Fails closed (exit 1) when the key is unset,
to never publish an unsigned artifact.
"""
import base64
import os
import sys

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

KEY_ENV = "GLYNDOR_RELEASE_ED25519_KEY"


def main() -> None:
	if len(sys.argv) < 2:
		print(__doc__, file=sys.stderr)
		sys.exit(1)

	seed_b64 = os.environ.get(KEY_ENV, "").strip()
	if not seed_b64:
		print(
			f"{KEY_ENV} is not set; refusing to publish an unsigned artifact",
			file=sys.stderr,
		)
		sys.exit(1)

	input_file = sys.argv[1]
	sig_file = sys.argv[2] if len(sys.argv) > 2 else input_file + ".sig"

	# Normalise padding to a 4-char boundary (the seed is stored unpadded), then
	# validate=True rejects a non-base64 seed (e.g. URL-safe -/_ or a stray char)
	# instead of silently discarding it and signing with a wrong key that nothing
	# can verify. The length check is defence-in-depth over from_private_bytes.
	pad = "=" * (-len(seed_b64) % 4)
	try:
		seed = base64.b64decode(seed_b64 + pad, validate=True)
	except (ValueError, base64.binascii.Error) as exc:
		print(f"{KEY_ENV} is not valid base64: {exc}", file=sys.stderr)
		sys.exit(1)
	if len(seed) != 32:
		print(f"{KEY_ENV} must decode to 32 bytes, got {len(seed)}", file=sys.stderr)
		sys.exit(1)

	with open(input_file, "rb") as f:
		data = f.read()

	sig = Ed25519PrivateKey.from_private_bytes(seed).sign(data)

	with open(sig_file, "wb") as f:
		f.write(sig)

	print(f"signed {input_file} ({len(data):,} bytes) → {sig_file} ({len(sig)} bytes)")


if __name__ == "__main__":
	main()
