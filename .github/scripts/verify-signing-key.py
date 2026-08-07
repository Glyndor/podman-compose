#!/usr/bin/env python3
"""Prove a freshly signed artifact verifies against the keys consumers embed.

Usage:
  verify-signing-key.py <signed-file> [<sig-file>] [<installer-path>]

sign.py checks only that GLYNDOR_RELEASE_ED25519_KEY is base64 that decodes to
32 bytes. A well-formed but WRONG seed therefore signs happily, and the release
publishes with a signature no installer, updater or apt client can verify - a
dead release, and releases are immutable, so the version is burnt and the fix is
a whole new one. The 2026-07 rotation made that concrete: the signing secret and
the embedded keys are edited in different places at different times, and nothing
compared them.

This closes the loop by verifying the produced signature against the public keys
that ship to users, read straight out of install.sh rather than restated here -
a copy in this file could drift from the installer and would prove nothing about
what a user actually runs. Any populated slot may verify (rotation trusts two
keys at once, and a third slot is the documented make-before-break target); the
check fails only when NO embedded key accepts the signature, which is exactly
the condition that would strand every consumer.

The optional third argument points the verifier at an installer other than the
one at the repo root - the test fixture ships a three-slot install.sh that the
slot-regex test exercises without touching the real installer.

Exit: 0 verified, 1 mismatch or malformed input.
"""
import base64
import re
import sys
from pathlib import Path

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey

DEFAULT_INSTALLER = Path(__file__).resolve().parents[2] / "install.sh"

# Matches every installer slot (`PODUP_RELEASE_PUBKEY_B64`,
# `PODUP_RELEASE_PUBKEY2_B64`, `PODUP_RELEASE_PUBKEY3_B64`, ...). The `[0-9]?`
# accepts the no-digit form (slot 0) and any one decimal digit (slots 1-9), so
# the third rotation slot - the documented make-before-break target - is not
# silently dropped. The default is taken from the `${VAR:-DEFAULT}` form so CI
# reads the same literal a user gets when the env override is unset.
SLOT_RE = re.compile(
	r'^PODUP_RELEASE_PUBKEY[0-9]?_B64="\$\{PODUP_RELEASE_PUBKEY[0-9]?_B64:-([^}]*)\}"$',
	re.MULTILINE,
)


def embedded_pubkeys(installer: Path) -> list[str]:
	"""Return the non-empty release pubkeys the installer ships, in slot order."""
	if not installer.is_file():
		sys.exit(f"cannot read {installer}; refusing to publish unverified")
	slots = SLOT_RE.findall(installer.read_text(encoding="utf-8"))
	if not slots:
		sys.exit(f"found no PODUP_RELEASE_PUBKEY*_B64 slots in {installer}")
	return [s for s in slots if s]


def main() -> None:
	if len(sys.argv) < 2:
		print(__doc__, file=sys.stderr)
		sys.exit(1)

	signed = Path(sys.argv[1])
	sig = Path(sys.argv[2]) if len(sys.argv) > 2 else Path(str(signed) + ".sig")
	installer = Path(sys.argv[3]) if len(sys.argv) > 3 else DEFAULT_INSTALLER

	data = signed.read_bytes()
	signature = sig.read_bytes()

	keys = embedded_pubkeys(installer)
	if not keys:
		sys.exit("every embedded key slot is empty; nothing could verify this release")

	for slot, key_b64 in enumerate(keys):
		pad = "=" * (-len(key_b64) % 4)
		try:
			raw = base64.b64decode(key_b64 + pad, validate=True)
			Ed25519PublicKey.from_public_bytes(raw).verify(signature, data)
		except (ValueError, base64.binascii.Error) as exc:
			sys.exit(f"embedded key slot {slot} is not a valid Ed25519 public key: {exc}")
		except InvalidSignature:
			continue
		print(f"{signed.name}: verified against embedded key slot {slot} ({key_b64})")
		return

	sys.exit(
		f"{signed.name}: the signature GLYNDOR_RELEASE_ED25519_KEY just produced "
		f"verifies against NONE of the {len(keys)} embedded key(s): {', '.join(keys)}.\n"
		"The signing secret and the keys baked into install.sh / install.ps1 / "
		"internal/update/verify.rs disagree, so this release would be unverifiable "
		"and uninstallable. Refusing to publish. Fix whichever side is wrong before "
		"retagging — the tag is immutable once the release exists."
	)


if __name__ == "__main__":
	main()
