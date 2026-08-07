#!/usr/bin/env bash
# Fixture install.sh for the release-signing regression test.
# Three rotation slots so the SLOT_RE change in verify-signing-key.py
# is exercised end-to-end. Slot 1 and slot 2 hold keypairs that do NOT
# match the test signatures; slot 3 holds the test key that signs every
# other asset in this directory.

PODUP_RELEASE_PUBKEY_B64="${PODUP_RELEASE_PUBKEY_B64:-iojj3XQJ8ZX9UtstPLpdcspnCb8dlBIb83SIAbQPb1w}"
PODUP_RELEASE_PUBKEY2_B64="${PODUP_RELEASE_PUBKEY2_B64:-gTl3Dqh9F19Wo1Rmw0x+zMuNipG07jeiXfYPW4/Js5Q}"
PODUP_RELEASE_PUBKEY3_B64="${PODUP_RELEASE_PUBKEY3_B64:-6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw}"

PUBKEYS=()
[[ -n "$PODUP_RELEASE_PUBKEY_B64"  ]] && PUBKEYS+=("$PODUP_RELEASE_PUBKEY_B64")
[[ -n "$PODUP_RELEASE_PUBKEY2_B64" ]] && PUBKEYS+=("$PODUP_RELEASE_PUBKEY2_B64")
[[ -n "$PODUP_RELEASE_PUBKEY3_B64" ]] && PUBKEYS+=("$PODUP_RELEASE_PUBKEY3_B64")
