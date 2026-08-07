#!/usr/bin/env bash
#
# podup installer.
#
# Default: download a release binary, verify it and install it to a directory.
# Updates come from `podup update`.
#
#   curl -fsSL https://glyndor.net/podup/install/unix | bash
#
# --apt (Debian/Ubuntu, amd64/arm64): set up the Glyndor apt repository and install
# with apt. Updates - including signing-key renewals - come from `apt upgrade`.
#
#   curl -fsSL https://glyndor.net/podup/install/unix | bash -s -- --apt
#
# Environment:
#   PODUP_VERSION      Release tag to install (e.g. v0.3.0). Default: latest.
#   PODUP_INSTALL_DIR  Installation directory (binary mode). Default: /usr/local/bin.

set -euo pipefail

REPO="Glyndor/podup"
INSTALL_DIR="${PODUP_INSTALL_DIR:-/usr/local/bin}"
VERSION="${PODUP_VERSION:-latest}"
MODE="binary"

log_info()  { printf '\033[1;34m[info]\033[0m %s\n' "$1"; }
log_ok()    { printf '\033[1;32m[ ok ]\033[0m %s\n' "$1"; }
log_error() { printf '\033[1;31m[fail]\033[0m %s\n' "$1" >&2; }

fail() {
	log_error "$1"
	exit 1
}

usage() {
	sed -n '3,17p' "$0" | sed 's/^# \{0,1\}//'
	exit 0
}

# --- Arguments ---------------------------------------------------------------

for arg in "$@"; do
	case "$arg" in
		--apt)      MODE="apt" ;;
		-h|--help)  usage ;;
		*)          fail "Unknown argument: ${arg} (try --help)" ;;
	esac
done

# --- Platform detection ------------------------------------------------------

case "$(uname -s)" in
	Linux)  OS="linux" ;;
	Darwin) OS="darwin" ;;
	*)      fail "Unsupported OS: $(uname -s) (supported: Linux, macOS)" ;;
esac

case "$(uname -m)" in
	x86_64)          ARCH="x86_64" ;;
	aarch64 | arm64) ARCH="arm64" ;;
	*)               fail "Unsupported architecture: $(uname -m) (supported: x86_64, arm64)" ;;
esac

ARTIFACT="podup-${OS}-${ARCH}"

# --- Shared download / verification ------------------------------------------

command -v curl >/dev/null 2>&1 || fail "curl is required"

# macOS ships shasum instead of sha256sum.
if command -v sha256sum >/dev/null 2>&1; then
	SHA256_CMD=(sha256sum)
elif command -v shasum >/dev/null 2>&1; then
	SHA256_CMD=(shasum -a 256)
else
	fail "sha256sum or shasum is required"
fi

if [[ "$VERSION" == "latest" ]]; then
	BASE_URL="https://github.com/${REPO}/releases/latest/download"
elif [[ "$VERSION" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
	BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
else
	fail "PODUP_VERSION must be 'latest' or a semver tag like v1.2.3, got: ${VERSION}"
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

download() {
	# download <url> <dest>
	#
	# --proto '=https' only restricts the initial URL: GitHub release assets
	# redirect to its CDN, and that redirect is governed by --proto-redir, not
	# --proto, so it needs its own pin - this is the fix that actually closes
	# the downgrade (an unpinned redirect could otherwise fall back to http).
	# --max-filesize caps the response at 200 MB, but only on curl that honours
	# it: it aborts mid-stream on curl >= 8.4.0, or on any curl version when the
	# server sends Content-Length. It does NOT bound the size on an older curl
	# (still the default on several supported distros) talking to a server that
	# omits Content-Length - that combination is not covered by this flag.
	# --max-time is the version-independent backstop: it caps the whole request
	# at 300s regardless of curl version or response headers, so a stalled or
	# never-ending response cannot hang the installer.
	curl --proto '=https' --proto-redir '=https' --tlsv1.2 \
		--max-filesize 209715200 --max-time 300 \
		-fsSL -o "$2" "$1" || fail "Download failed: $1"
}

# Baked-in base64 (unpadded) raw Ed25519 public keys (32 bytes each) matching the
# release signing keys (GLYNDOR_RELEASE_ED25519_KEY / _2). Both are populated so a
# release signed by either key verifies during a make-before-break rotation; the
# signature passes if any key validates. Retire a key by clearing its slot here.
# Override for a fork via the PODUP_RELEASE_PUBKEY_B64 / _PUBKEY2_B64 env vars.
PODUP_RELEASE_PUBKEY_B64="${PODUP_RELEASE_PUBKEY_B64:-HFv7vg5FCY7YyKUDbJhaQSfB9SboJGSblJtFbLmLHzM}"
PODUP_RELEASE_PUBKEY2_B64="${PODUP_RELEASE_PUBKEY2_B64:-}"

PUBKEYS=()
[[ -n "$PODUP_RELEASE_PUBKEY_B64" ]]  && PUBKEYS+=("$PODUP_RELEASE_PUBKEY_B64")
[[ -n "$PODUP_RELEASE_PUBKEY2_B64" ]] && PUBKEYS+=("$PODUP_RELEASE_PUBKEY2_B64")

# Verify the Ed25519 signature over SHA256SUMS.
#   ed25519_verify <sig-file> <data-file>
# Exit: 0 verified, 1 signature present but INVALID (tampered),
#       2 cannot verify (no python3/cryptography, no key configured),
#       3 the configured release key is malformed (a configuration problem,
#         not a release-tampering problem - kept distinct from rc=1 so the
#         caller can report it without scaring the user about the release).
ed25519_verify() {
	[[ ${#PUBKEYS[@]} -gt 0 ]] || return 2
	command -v python3 >/dev/null 2>&1 || return 2
	python3 -c "from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey" 2>/dev/null || return 2
	python3 - "$1" "$2" "${PUBKEYS[@]}" <<'PYEOF'
import base64, binascii, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from cryptography.exceptions import InvalidSignature
sig_file, data_file = sys.argv[1], sys.argv[2]
sig = open(sig_file, "rb").read()
data = open(data_file, "rb").read()
for slot, pubkey_b64 in enumerate(sys.argv[3:]):
    try:
        # Pad to a 4-byte boundary the way sign.py does: the installer stores
        # the key unpadded, and a stricter decoder would reject a fixed two-"="
        # suffix when the key is already a multiple of four chars long.
        raw = base64.b64decode(pubkey_b64 + "=" * (-len(pubkey_b64) % 4))
        Ed25519PublicKey.from_public_bytes(raw).verify(sig, data)
        sys.exit(0)
    except (binascii.Error, ValueError) as exc:
        # The key is set but cannot be decoded into 32 bytes. This is a
        # configuration problem (a bad PODUP_RELEASE_PUBKEY*_B64 override), not
        # a release-tampering problem - do not lump it in with rc=1.
        print(f"configured release key slot {slot} is malformed: {exc}", file=sys.stderr)
        sys.exit(3)
    except InvalidSignature:
        continue
sys.exit(1)
PYEOF
	# Preserve the Python exit code: 0 verified, 3 malformed key, 1 mismatch.
	# An `if/then/return 1` wrapper would fold rc=3 into rc=1, defeating the
	# L7 distinction the install.ps1 side already exposes.
	case $? in
		0) return 0 ;;
		3) return 3 ;;
		*) return 1 ;;
	esac
}

# Confirm a downloaded file matches its entry in SHA256SUMS.
#   verify_checksum <name>
verify_checksum() {
	log_info "Verifying SHA-256 checksum ..."
	(cd "$TMP_DIR" && grep " $1\$" SHA256SUMS | "${SHA256_CMD[@]}" -c --quiet -) \
		|| fail "Checksum verification failed for $1"
	log_ok "Checksum verified"
}

run_root() {
	if [[ "$(id -u)" -eq 0 ]]; then
		"$@"
	elif command -v sudo >/dev/null 2>&1; then
		sudo "$@"
	else
		fail "root privileges required and sudo is not available for: $*"
	fi
}

# --- apt mode ----------------------------------------------------------------

install_apt() {
	[[ "$OS" == "linux" ]] || fail "--apt is only supported on Debian/Ubuntu Linux"
	command -v apt-get >/dev/null 2>&1 || fail "--apt requires apt-get (Debian/Ubuntu)"
	command -v dpkg >/dev/null 2>&1 || fail "--apt requires dpkg"

	# Bootstrap the repository over HTTPS by installing the glyndor-archive-keyring
	# package, which registers the signing key and source list. The package is
	# verified against the embedded Ed25519 release key before it is installed, so
	# a tampered CDN or MITM cannot inject a rogue keyring. From then on apt verifies
	# every package against the repository's GPG signature, and key renewals arrive
	# automatically through apt upgrade.
	local kr="glyndor-archive-keyring.deb"
	log_info "Setting up the Glyndor apt repository ..."
	download "https://apt.glyndor.net/${kr}" "${TMP_DIR}/${kr}"
	download "https://apt.glyndor.net/${kr}.sig" "${TMP_DIR}/${kr}.sig"

	# Fail closed: the keyring package is installed as root, so it must carry a
	# valid Ed25519 signature from the release key. No checksum-only fallback.
	log_info "Verifying keyring package signature ..."
	local kr_rc=0
	ed25519_verify "${TMP_DIR}/${kr}.sig" "${TMP_DIR}/${kr}" || kr_rc=$?
	case "$kr_rc" in
		0) log_ok "Keyring signature verified" ;;
		1) fail "Keyring signature verification failed - aborting (package may be tampered)" ;;
		2) fail "Cannot verify keyring signature: install python3 with the 'cryptography' package (and a configured release key) - aborting" ;;
		3) fail "Configured release key is malformed - check PODUP_RELEASE_PUBKEY_B64 / _PUBKEY2_B64 values and re-run" ;;
	esac

	run_root dpkg -i "${TMP_DIR}/${kr}"
	run_root apt-get update
	log_info "Installing podup ..."
	run_root apt-get install -y podup

	log_ok "podup installed via apt: $(podup --version)"
	log_info "Future updates: sudo apt upgrade"
}

# --- binary mode -------------------------------------------------------------

install_binary() {
	log_info "Downloading ${ARTIFACT} (${VERSION}) ..."
	download "${BASE_URL}/${ARTIFACT}" "${TMP_DIR}/${ARTIFACT}"
	download "${BASE_URL}/SHA256SUMS" "${TMP_DIR}/SHA256SUMS"
	download "${BASE_URL}/SHA256SUMS.sig" "${TMP_DIR}/SHA256SUMS.sig"

	# Checksum alone is not a trust anchor: a tampered release can ship a matching
	# SHA256SUMS. The binary is trusted only after at least one cryptographic proof
	# tied to the release key or the repository's build identity succeeds - the
	# Ed25519 signature over SHA256SUMS, or the GitHub build-provenance attestation.
	# If neither verifier can run, the install fails closed.
	local verified=0 rc=0

	log_info "Verifying SHA256SUMS signature ..."
	ed25519_verify "${TMP_DIR}/SHA256SUMS.sig" "${TMP_DIR}/SHA256SUMS" || rc=$?
	case "$rc" in
		0) log_ok "SHA256SUMS signature verified"; verified=1 ;;
		1) fail "SHA256SUMS signature verification failed - release may be tampered" ;;
		2)
			if [[ ${#PUBKEYS[@]} -eq 0 ]]; then
				log_info "no release public key configured - skipping Ed25519 signature check"
			else
				# A release public key IS configured: the pinned key is the trust
				# anchor and must not be silently bypassed in favour of the
				# (repo-scoped) attestation. Fail closed.
				fail "python3 with the 'cryptography' package is required to verify the \
release signature against the pinned key. Install it and re-run."
			fi
			;;
		3) fail "Configured release key is malformed - check PODUP_RELEASE_PUBKEY_B64 / _PUBKEY2_B64 values and re-run" ;;
	esac

	# Build-provenance attestation: proves the binary was produced by this repo's
	# release workflow (GitHub OIDC). Defence-in-depth next to the pinned key;
	# the trust anchor when no release public key is configured. Pinned to the
	# release workflow - a repo-scoped check would accept an attestation from any
	# workflow in the repo.
	if command -v gh >/dev/null 2>&1 && gh attestation --help >/dev/null 2>&1; then
		log_info "Verifying artifact attestation ..."
		gh attestation verify "${TMP_DIR}/${ARTIFACT}" --repo "$REPO" \
			--signer-workflow "${REPO}/.github/workflows/release.yml" >/dev/null \
			|| fail "Attestation verification failed for ${ARTIFACT}"
		log_ok "Attestation verified"
		verified=1
	else
		log_info "GitHub CLI with attestation support not found - cannot check attestation"
	fi

	# Fail closed: a strong cryptographic proof is mandatory. A checksum alone is
	# not a trust anchor, and there is no opt-out - government and hardened
	# environments require verifiable supply-chain integrity at install time.
	if [[ "$verified" -ne 1 ]]; then
		fail "No signature or attestation verifier available. Install 'gh' (>= 2.49) \
or python3 with the 'cryptography' package, or set PODUP_RELEASE_PUBKEY_B64, then re-run."
	fi

	verify_checksum "$ARTIFACT"

	# Stage next to the target and rename into place, so a kill mid-install can
	# never leave a partial yet executable binary on PATH.
	local staged="${INSTALL_DIR}/.podup.install-$$"
	local install_cmd=(install -m 0755 "${TMP_DIR}/${ARTIFACT}" "$staged")
	local move_cmd=(mv -f "$staged" "${INSTALL_DIR}/podup")
	if [[ -w "$INSTALL_DIR" ]]; then
		"${install_cmd[@]}" && "${move_cmd[@]}"
	elif command -v sudo >/dev/null 2>&1; then
		log_info "Installing to ${INSTALL_DIR} (requires sudo) ..."
		sudo "${install_cmd[@]}" && sudo "${move_cmd[@]}"
	else
		fail "Cannot write to ${INSTALL_DIR} and sudo is not available. Set PODUP_INSTALL_DIR to a writable directory."
	fi

	log_ok "podup installed: $("${INSTALL_DIR}/podup" --version)"
}

# --- Dispatch ----------------------------------------------------------------

if [[ "$MODE" == "apt" ]]; then
	install_apt
else
	install_binary
fi
