#!/usr/bin/env bash
# Regression test for the version self-test in install.sh (issue #1356).
#
# Closes the rollback window in the shell installer: a CDN or
# transparent-proxy replay can serve an older, *legitimately* signed
# binary and matching SHA256SUMS - both still verify. The version
# self-test pins the staged binary's reported --version to the resolved
# tag, refusing installs a Rust reference already refuses
# (internal/update/install.rs:152-205).
#
# Cases:
#   1. Reports the resolved tag with v prefix -> self-test passes.
#   2. Reports the resolved tag without v prefix -> self-test passes.
#   3. Reports an older tag -> self-test fails.
#   4. Reports a -dev suffix on the right version -> self-test fails
#      (the rollback case: a partial matches a full token but not the
#      exact one).
#   5. Reports garbage -> self-test fails.
#   6. Exits non-zero on --version -> self-test fails.
#   7. The staged file does not exist -> self-test fails.
#   8. On any failed self-test the staged file is removed.
#
# Run from the repo root:
#   bash tests/fixtures/releases/version-self-test.sh
set -euo pipefail

# __file__ is tests/fixtures/releases/version-self-test.sh; walk three
# levels up to land on the repo root regardless of where the script is
# invoked from.
REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
INSTALL_SH="$REPO_ROOT/install.sh"

# Source the helpers (verify_version_self_test, fail, log_info, log_ok,
# log_error, REPO, ...) by stripping the Dispatch section. Everything
# before the dispatch is pure function definitions and constants; sourcing
# it does not touch the network or write to the filesystem. Same pattern
# the existing test.sh uses.
TMP_HELPERS="$(mktemp)"
trap 'rm -f "$TMP_HELPERS"' EXIT
sed '/^# --- Dispatch /,$d' "$INSTALL_SH" > "$TMP_HELPERS"
# shellcheck disable=SC1090
source "$TMP_HELPERS"

# Stub directory: tiny executables that print specific --version outputs.
STUBS="$(mktemp -d)"
trap 'rm -rf "$TMP_HELPERS" "$STUBS"' EXIT

write_stub() {
	local name="$1" body="$2"
	local path="$STUBS/$name"
	printf '%s\n' "$body" > "$path"
	chmod +x "$path"
	printf '%s' "$path"
}

# Run verify_version_self_test against a stub that is expected to PASS.
# The function calls `fail` (which does `exit 1`) on any unexpected
# failure, so a passing run is the only path that reaches the next assertion.
assert_pass() {
	local stub_path="$1" tag="$2"
	if verify_version_self_test "$stub_path" "$tag"; then
		echo "  OK    $stub_path reports the resolved tag"
	else
		echo "  FAIL  $stub_path should have passed but was refused" >&2
		exit 1
	fi
}

# Run verify_version_self_test against a stub that is expected to FAIL.
# We run inside a subshell so the function's `exit 1` only kills the
# subshell, not the test script, and we can read $? afterwards. After
# the run we also confirm the staged file is gone (the rollback path
# removes the staged binary so a kill at this point cannot leave a
# partial yet executable binary on PATH).
assert_fail() {
	local stub_path="$1" tag="$2"
	set +e
	(verify_version_self_test "$stub_path" "$tag") >/dev/null 2>&1
	rc=$?
	set -e
	if [[ $rc -eq 0 ]]; then
		echo "  FAIL  $stub_path should have been refused but the self-test accepted it" >&2
		exit 1
	fi
	if [[ -e "$stub_path" ]]; then
		echo "  FAIL  staged file $stub_path was not removed after the failed self-test" >&2
		exit 1
	fi
	echo "  OK    $stub_path was refused and the staged file was removed"
}

TAG="v3.7.0"

echo "Part 1: --version reports the resolved tag (with v prefix)"
stub="$(write_stub 'pass_v' \
	'#!/bin/sh
echo "podup version v3.7.0"
exit 0')"
assert_pass "$stub" "$TAG"

echo "Part 2: --version reports the resolved tag (without v prefix)"
stub="$(write_stub 'pass_plain' \
	'#!/bin/sh
echo "podup 3.7.0"
exit 0')"
assert_pass "$stub" "$TAG"

echo "Part 3: --version reports an older tag (rollback)"
stub="$(write_stub 'fail_older' \
	'#!/bin/sh
echo "podup version v3.6.0"
exit 0')"
assert_fail "$stub" "$TAG"

echo "Part 4: --version reports a -dev suffix on the resolved version"
stub="$(write_stub 'fail_dev' \
	'#!/bin/sh
echo "podup version v3.7.0-dev"
exit 0')"
assert_fail "$stub" "$TAG"

echo "Part 5: --version reports garbage"
stub="$(write_stub 'fail_garbage' \
	'#!/bin/sh
echo "definitely not a podup"
exit 0')"
assert_fail "$stub" "$TAG"

echo "Part 6: --version exits non-zero"
stub="$(write_stub 'fail_exit' \
	'#!/bin/sh
echo "podup version v3.7.0"
exit 1')"
assert_fail "$stub" "$TAG"

echo "Part 7: staged file does not exist"
assert_fail "$STUBS/does-not-exist" "$TAG"

echo
echo "All parts passed."