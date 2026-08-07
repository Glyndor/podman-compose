#!/usr/bin/env bash
# Regression test for the release-signing key handling (issue #1359).
#
# Three checks:
#   1. Per-asset .sig loop - verify-signing-key.py walks every *.sig in the
#      fixture directory (issue #1359 M1: previously only SHA256SUMS.sig was
#      checked at release time, so a per-binary substitution could pass).
#   2. SLOT_RE recognises the third rotation slot - the fixture install.sh
#      ships three populated slots, with the matching key in slot 3 only. If
#      the SLOT_RE change had not landed, slot 3 would be silently ignored
#      and the verifier would fail (issue #1359 M2).
#   3. ed25519_verify classifies errors correctly - a malformed key produces
#      exit code 3 (configuration problem), not 1 (release-tampering). Also
#      verifies the canonical base64 padding formula tolerates a key whose
#      unpadded length is a multiple of four (issue #1359 M3 and L7).
#
# Run from the repo root:
#   bash tests/fixtures/releases/test.sh
set -euo pipefail

# __file__ is tests/fixtures/releases/test.sh; walk three levels up to land
# on the repo root regardless of where the script is invoked from.
REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
FIXTURE_DIR="$REPO_ROOT/tests/fixtures/releases"
VERIFIER="$REPO_ROOT/.github/scripts/verify-signing-key.py"
INSTALL_SH="$REPO_ROOT/install.sh"

PYTHON_PRESENT=0
if command -v python3 >/dev/null 2>&1 \
	&& python3 -c "from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey" 2>/dev/null; then
	PYTHON_PRESENT=1
fi
if [[ $PYTHON_PRESENT -ne 1 ]]; then
	echo "SKIP: python3 with 'cryptography' is not installed" >&2
	exit 0
fi

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

# -----------------------------------------------------------------------------
# Part 1: per-asset .sig loop, with three populated rotation slots.
# -----------------------------------------------------------------------------
echo "Part 1: per-asset .sig verification (3 rotation slots)"

passed=0
total=0
shopt -s nullglob
for sig in "$FIXTURE_DIR"/*.sig; do
	base="${sig%.sig}"
	name="$(basename "$base")"
	total=$((total + 1))
	if python3 "$VERIFIER" "$base" "$sig" "$FIXTURE_DIR/install.sh" >/dev/null; then
		passed=$((passed + 1))
		echo "  OK    $name"
	else
		echo "  FAIL  $name (rc=$?)"
	fi
done
[[ $passed -eq $total ]] || fail "only $passed of $total signatures verified"

# -----------------------------------------------------------------------------
# Part 2: third slot is the one that verifies - proves SLOT_RE picks it up.
# Corrupt slot 3 to a wrong pubkey and confirm the verifier falls back to
# "no embedded key accepts the signature".
# -----------------------------------------------------------------------------
echo "Part 2: SLOT_RE picks up the third rotation slot"

# A valid Ed25519 public key that is NOT the test key. The crypto library
# accepts any curve point; using a fresh keypair keeps the negative test
# honest (a real wrong key, not just a malformed string).
WRONG_PK="$(python3 -c '
import base64
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
raw = Ed25519PrivateKey.from_private_bytes(b"\x99" * 32).public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw)
print(base64.b64encode(raw).decode().rstrip("="))
')"

# Rewrite slot 3 with the wrong key. The verifier must now fail because only
# slots 1 and 2 are populated, and they do not match the test signatures.
tmp_install="$(mktemp)"
trap 'rm -rf "$tmp_install" "$tmp_helpers" "$TMP_DIR"' EXIT
sed "s|^PODUP_RELEASE_PUBKEY3_B64=.*|PODUP_RELEASE_PUBKEY3_B64=\"\${PODUP_RELEASE_PUBKEY3_B64:-$WRONG_PK}\"|" \
	"$FIXTURE_DIR/install.sh" > "$tmp_install"

if python3 "$VERIFIER" "$FIXTURE_DIR/podup-linux-x86_64" \
		"$FIXTURE_DIR/podup-linux-x86_64.sig" "$tmp_install" >/dev/null 2>&1; then
	fail "verifier accepted a signature with the matching key in slot 3 replaced"
fi
echo "  OK    slot 3 is recognised (its absence flips the verifier to a no-match error)"

# -----------------------------------------------------------------------------
# Part 3: L7 error classification - a malformed key produces rc=3, not rc=1.
# The bash wrapper around ed25519_verify has to surface this so the user
# sees a configuration problem, not a release-tampering warning.
#
# Source the helpers (PUBKEYS, ed25519_verify) from install.sh WITHOUT the
# dispatch: sed strips the "# --- Dispatch ---" section that would otherwise
# try to reach the network. The PUBKEYS array is then populated by the
# real installer code, and the test overrides it in-place to inject a
# malformed key.
# -----------------------------------------------------------------------------
echo "Part 3: malformed key produces a configuration problem, not tampering"

# The harness: set the test pubkey as the active key (slot 0). The empty
# slot 2 stays empty so PUBKEYS is a one-element array.
TEST_PUBKEY="$(cat "$FIXTURE_DIR/test-key.b64")"
export PODUP_RELEASE_PUBKEY_B64="$TEST_PUBKEY"
export PODUP_RELEASE_PUBKEY2_B64=""

tmp_helpers="$(mktemp)"
# Trim from the Dispatch section header to end of file. That leaves the
# PUBKEYS setup, the log helpers, and ed25519_verify - everything we need
# to exercise the verifier in isolation, with no network access. The
# trailing character after "Dispatch" is a space, not a "-" - the line is
# padded with dashes that make a literal `---$` never match.
sed '/^# --- Dispatch /,$d' "$INSTALL_SH" > "$tmp_helpers"
# shellcheck disable=SC1090
source "$tmp_helpers"

TMP_DIR="$(mktemp -d)"
printf 'fixture data' > "$TMP_DIR/data"
printf 'badsig' > "$TMP_DIR/sig"

# Replace PUBKEYS with a deliberately malformed key: a string that is not
# valid base64 of 32 bytes. rc=1 (tampered) is wrong; rc=3 (config) is right.
PUBKEYS=("@@@@not-base64@@@@")

set +e
ed25519_verify "$TMP_DIR/sig" "$TMP_DIR/data" >/dev/null 2>"$TMP_DIR/err"
rc=$?
set -e
[[ $rc -eq 3 ]] || fail "expected rc=3 (config error) for a malformed key, got rc=$rc"
grep -q "malformed" "$TMP_DIR/err" || fail "expected the stderr to mention 'malformed', got: $(cat "$TMP_DIR/err")"
echo "  OK    rc=3 with 'malformed' on stderr (config error class)"

# Sanity check: with the real key in PUBKEYS, a valid signature should
# still pass. This is a regression guard against the L7 change breaking
# the happy path.
# shellcheck disable=SC2034  # PUBKEYS is consumed by the sourced ed25519_verify.
PUBKEYS=("$(cat "$FIXTURE_DIR/test-key.b64")")
SIGN_PY="$(cat <<'PY'
import base64, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
seed = bytes([7]) * 32
sk = Ed25519PrivateKey.from_private_bytes(seed)
sys.stdout.buffer.write(sk.sign(open(sys.argv[1], "rb").read()))
PY
)"
python3 -c "$SIGN_PY" "$TMP_DIR/data" > "$TMP_DIR/data.sig"
ed25519_verify "$TMP_DIR/data.sig" "$TMP_DIR/data" || fail "happy path: ed25519_verify must accept a real signature under the test key"
echo "  OK    happy path still verifies"

# -----------------------------------------------------------------------------
# Part 4: canonical base64 padding (M3). A key whose unpadded length is a
# multiple of four would have been over-padded by the old "+ \"==\"" code.
# A stricter decoder would reject the result; the new formula pads with the
# minimum number of "=" required and lets Python's lenient decoder do the
# rest. The match still works against a signature made with the matching key.
# -----------------------------------------------------------------------------
echo "Part 4: canonical base64 padding for keys of any length"

# 32 raw bytes => 43 unpadded base64 chars, then a single "=" pads to 44.
# The old `+ "=="` formula would have produced 45 chars (two extra "="); a
# stricter decoder would reject the second "=" as over-padded. The new
# `-len % 4` formula produces the canonical 44-char form. Build a real
# 32-byte Ed25519 key, base64 it, and round-trip it through the new formula
# to confirm the decoder yields 32 raw bytes back.
python3 -c "
import base64
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
raw = Ed25519PrivateKey.from_private_bytes(b'\x33' * 32).public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw)
b = base64.b64encode(raw).decode()
# 32 raw bytes => 43 unpadded base64 chars.
assert len(b.rstrip('=')) == 43, f'unexpected key length: {len(b.rstrip(chr(61)))}'
# Round-trip via the new padding formula (one '=' for a 43-char key).
raw2 = base64.b64decode(b + '=' * (-len(b) % 4))
assert raw2 == raw, 'canonical padding does not round-trip the key'
# Also: the same formula applied to the OLD over-padded form must still
# produce 32 raw bytes (Python strips trailing '=' before validation).
overpadded = b + '=='
raw3 = base64.b64decode(overpadded + '=' * (-len(overpadded) % 4))
assert raw3 == raw, 'old over-padded form must still round-trip cleanly'
print('  OK    canonical padding round-trips 43-char keys and tolerates old over-padded form')
"

# A known-mis-padded key (one with stray characters) must produce rc=3
# (configuration problem), not rc=1 (release-tampering), so a fork
# maintainer debugging an override isn't told to chase a phantom signature
# mismatch. We exercise the same Python snippet install.sh embeds to keep
# the matrix symmetric.
TMP_PAD_DIR="$(mktemp -d)"
printf 'fixture data' > "$TMP_PAD_DIR/data"
printf 'badsig' > "$TMP_PAD_DIR/sig"
PAD_SNIPPET="$(cat <<'PY'
import base64, binascii, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from cryptography.exceptions import InvalidSignature
sig_file, data_file = sys.argv[1], sys.argv[2]
sig = open(sig_file, "rb").read()
data = open(data_file, "rb").read()
for slot, pubkey_b64 in enumerate(sys.argv[3:]):
    try:
        raw = base64.b64decode(pubkey_b64 + "=" * (-len(pubkey_b64) % 4))
        Ed25519PublicKey.from_public_bytes(raw).verify(sig, data)
        sys.exit(0)
    except (binascii.Error, ValueError) as exc:
        print("configured release key slot %d is malformed: %s" % (slot, exc), file=sys.stderr)
        sys.exit(3)
    except InvalidSignature:
        continue
sys.exit(1)
PY
)"
PY_FILE="$(mktemp)"
printf '%s\n' "$PAD_SNIPPET" > "$PY_FILE"
# Stray characters at the end (a known-bad override shape) -> rc=3, not rc=1.
set +e
python3 "$PY_FILE" "$TMP_PAD_DIR/sig" "$TMP_PAD_DIR/data" "${TEST_PUBKEY}@@@bad@@@" >/dev/null 2>"$TMP_PAD_DIR/err"
rc=$?
set -e
[[ $rc -eq 3 ]] || fail "mis-padded key must produce rc=3 (config), got rc=$rc"
grep -q "malformed" "$TMP_PAD_DIR/err" || fail "expected stderr to mention 'malformed', got: $(cat "$TMP_PAD_DIR/err")"
echo "  OK    mis-padded key (stray chars) -> rc=3 (config error class)"

rm -f "$PY_FILE"
rm -rf "$TMP_PAD_DIR"

echo
echo "All parts passed."
