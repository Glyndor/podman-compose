#!/usr/bin/env bash
#
# Behaviour test for install.sh's refusal path (#1746 entry 1).
#
# When install.sh refuses a staged binary because its --version does not match
# the resolved release tag, the staged file in INSTALL_DIR is supposed to be
# removed. The original code calls `rm -f "$staged"` directly, which runs as
# the calling user. In the realistic flow the staged file is owned by root
# (because the copy was done through `sudo sh -c "... && chmod ..."`), so the
# unprivileged `rm` fails silently. The user is then told "the staged file
# has been removed" while the binary sits in /usr/local/bin owned by root.
#
# The fix is to route the cleanup through `run_root`, so the rm is elevated
# the same way the install itself was.
#
# The assertion here is direct: the function's refusal path must invoke
# `sudo rm -f` (or equivalent privilege elevation), not a bare `rm -f`. The
# stubs below record calls without altering behaviour so the difference is
# visible.
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALL_SH="$HERE/install.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0

check() { # <description> <expected> <actual>
	if [ "$2" = "$3" ]; then
		echo "ok    $1"
		pass=$((pass + 1))
	else
		echo "FAIL  $1"
		echo "        expected: $2"
		echo "        actual:   $3"
		fail=$((fail + 1))
	fi
}

# --- stubs -------------------------------------------------------------------
#
# Stub `sudo` to record the call and then run the command as the caller. The
# stub does not actually need elevated privileges for this assertion: the
# point is that `sudo` was invoked at all. In the buggy version, `run_root`
# is not in the call chain at all, so `sudo` is never recorded.
STUB="$WORK/stub"
mkdir -p "$STUB"

cat > "$STUB/sudo" <<'EOF'
#!/bin/sh
printf 'SUDO %s\n' "$*" >> "$WORK/sudo.log"
exec "$@"
EOF
chmod +x "$STUB/sudo"

# Stub `rm` so the call is recorded even when the underlying rm would succeed.
# Real `rm` would happily remove a file the test owns; this stub records the
# call before delegating, so the test sees what was invoked.
cat > "$STUB/rm" <<'EOF'
#!/bin/sh
printf 'RM %s\n' "$*" >> "$WORK/rm.log"
exec /bin/rm "$@"
EOF
chmod +x "$STUB/rm"

# --- fixture -----------------------------------------------------------------
#
# A staged binary whose --version reports something that does not match the
# expected tag. The function under test compares each whitespace-delimited
# token in the output against the tag, so `podup version 0.0.0` against an
# expected `v9.9.9` is a mismatch and lands in the refusal branch.
STAGED="$WORK/.podup.install-$$"
cat > "$STAGED" <<'EOF'
#!/bin/sh
echo "podup version 0.0.0"
EOF
chmod +x "$STAGED"

# --- run ---------------------------------------------------------------------
#
# Source the two relevant functions from install.sh. log_ok / log_error / fail
# are overridden locally so the test captures the refusal message instead of
# exiting the process. run_root is sourced as-is so the fix has to take effect
# inside it; the bug is in verify_version_self_test not calling run_root, so
# sourcing the real run_root is correct for both versions.
# Both stdout and stderr are captured so the refusal message is observable,
# and the exit code is preserved (no `|| true` so $? reflects the fail() exit).
OUT="$(PATH="$STUB:$PATH" WORK="$WORK" STAGED="$STAGED" INSTALL_SH="$INSTALL_SH" \
	bash 2>&1 <<'INNER'
set +e

log_ok() { :; }
log_error() { :; }
fail() { printf 'REFUSAL %s\n' "$1"; exit 1; }

eval "$(awk '/^verify_version_self_test\(\) {/,/^}$/' "$INSTALL_SH")"
eval "$(awk '/^run_root\(\) {/,/^}$/' "$INSTALL_SH")"

verify_version_self_test "$STAGED" "v9.9.9"
INNER
)"
RC=$?

REFUSAL="$(printf '%s\n' "$OUT" | grep -E '^REFUSAL ' | head -n 1 | sed 's/^REFUSAL //')"

# --- assertions --------------------------------------------------------------

check 'verify_version_self_test refuses a wrong version (non-zero exit)' "1" "$RC"
check 'and the refusal message names the rollback cause' "1" \
	"$([ -n "$REFUSAL" ] && echo 1 || echo 0)"
check 'and the refusal message says the staged file has been removed' "1" \
	"$([ -n "$REFUSAL" ] && printf '%s' "$REFUSAL" | grep -q -F 'the staged file has been removed' && echo 1 || echo 0)"

# The substantive assertion: the cleanup of the staged file must go through
# `run_root`, which prefixes `sudo` when running as a non-root user. The bug
# is that the original code calls `rm -f "$staged"` directly, with no `sudo`
# in front, so a non-root caller running install.sh with the default
# /usr/local/bin target leaves the file behind.
SUDO_RM_COUNT=0
if [ -f "$WORK/sudo.log" ]; then
	SUDO_RM_COUNT="$(grep -c '^SUDO rm -f' "$WORK/sudo.log" || true)"
	SUDO_RM_COUNT="${SUDO_RM_COUNT:-0}"
fi
check 'the refusal path invokes sudo rm -f on the staged file' "1" "$SUDO_RM_COUNT"

# --- cleanup state -----------------------------------------------------------

# After the function returns (refused or accepted), the staged file must not
# exist on disk. With the stub `sudo` chaining to real `/bin/rm` this is the
# same outcome whether the fix is in place or not, so this check is the
# positive control rather than the bug detector.
check 'the staged file is gone after refusal' "0" \
	"$([ -e "$STAGED" ] && echo 1 || echo 0)"

echo
echo "$pass passed, $fail failed"
printf 'DONE %s %d %d\n' "${BASH_SOURCE[0]##*/}" "$pass" "$fail"
[ "$fail" -eq 0 ]
