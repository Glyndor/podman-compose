#!/usr/bin/env bash
#
# Behaviour tests for the pin-length assertion in
# .github/workflows/reusable-workflow-lint.yml: every third-party
# `uses:` reference must pin to exactly 40 hexadecimal characters.
#
# A commit SHA is 40 characters. GitHub answers 422 for any other
# length, so a workflow carrying a 39-character pin never starts the
# job that references it. In #1741 that cost the repository the only
# coverage of install.sh's refusal of an https-to-http redirect: the
# fixture was correct, the job named it, and the job had never run.
#
# The reason nothing caught it is the distinction this file exists to
# hold open. check-suite-completeness.test.sh and
# ci-runs-every-test.test.sh assert that every test is NAMED by a
# workflow, and a broken pin leaves the naming intact. Being named is
# not being run.
#
# Local reusable calls (`uses: ./.github/workflows/x.yml`) carry no
# `@` and so carry no pin to measure; they are out of scope by shape
# rather than by exception. An unpinned third-party reference
# (`uses: owner/repo@v4`) is a separate defect with its own control,
# and this assertion is about length only.
#
# The step's `run:` body is extracted from the workflow and executed
# as it ships, in a temporary tree shaped like a repository, so the
# test cannot drift from the thing it tests. Every refusal asserts
# WHICH message fired, and each refusal is paired with an acceptance
# of the same shape just inside the line.
#
# Requires: python3 (the step under test is a python3 heredoc).
#
# Several assertions match against text the step prints verbatim,
# backticks and all. Those needles are single-quoted so the characters
# reach `grep -F` unexpanded, which is the point rather than an
# oversight; the sibling caller-if test carries the same waiver.
# shellcheck disable=SC2016
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0

# Descriptions carry backticks, so they are passed single-quoted and
# the helper takes its argument without re-quoting it.
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

# Pull one step's 'run:' body out of a workflow, dedented, so it can
# be run. Same helper as the other two reusable-workflow-lint tests in
# this directory: the point is to exercise the script as it ships
# rather than a copy of it that can drift.
step_script() { # $1=workflow path  $2=step name substring
	python3 - "$1" "$2" <<'PY'
import sys
lines = open(sys.argv[1]).read().splitlines()
start = next(i for i, l in enumerate(lines) if sys.argv[2] in l)
run = next(i for i, l in enumerate(lines) if i > start and l.strip() == "run: |")
body = []
for line in lines[run + 1:]:
    if not line.strip():
        body.append("")
        continue
    if not line.startswith(" " * 10):
        break
    body.append(line[10:])
print("\n".join(body))
PY
}

WORKFLOW="$HERE/.github/workflows/reusable-workflow-lint.yml"
step_script "$WORKFLOW" "pin must be 40 hex" > "$WORK/pin-length.sh"
check 'the pin-length assertion was extracted from the workflow' "1" \
	"$(grep -c 'SHA1_RE' "$WORK/pin-length.sh" | awk '{print ($1>0)}')"

# ---------------------------------------------------------------------------
# Fixture helpers
# ---------------------------------------------------------------------------

# The real 40-character actions/checkout pin this repository uses
# everywhere, and the 39-character truncation of it that #1741 shipped
# (the second 'a' of 'e5aac' dropped).
GOOD_PIN="3d3c42e5aac5ba805825da76410c181273ba90b1"
BAD_PIN="3d3c42e5ac5ba805825da76410c181273ba90b1"

new() {
	rm -rf "$WORK/t"
	mkdir -p "$WORK/t/.github/workflows"
	printf '%s' "$WORK/t"
}
run_in() { # $1=dir $2=script -> stdout+stderr, status in $?
	( cd "$1" && bash "$2" 2>&1 )
}
said() { printf '%s' "$1" | grep -q -F -- "$2" && echo 1 || echo 0; }

# A one-job workflow whose single step pins actions/checkout to $2.
pinned() { # $1=dir $2=pin
	cat > "$1/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@$2 # v7.0.1
        with:
          persist-credentials: false
EOF
}

# ===========================================================================
# Positive control: the real tree passes. This is the assertion that
# went from red to green when line 194 of asset-contract.yml was
# repaired, and it is what keeps the repair from silently regressing.
# ===========================================================================

out="$(cd "$HERE" && bash "$WORK/pin-length.sh")"; rc=$?
check 'the real repository tree passes the pin-length assertion' "0" "$rc"
check 'and the step prints the OK summary, not a refusal' "1" \
	"$(said "$out" 'checkout-pin-length: every `uses:` pin')"

# ===========================================================================
# The defect of #1741, reproduced: a 39-character pin is refused, and
# the message names the file and the pin.
# ===========================================================================

d="$(new)"; pinned "$d" "$BAD_PIN"
out="$(run_in "$d" "$WORK/pin-length.sh")"; rc=$?
check 'a 39-character pin is refused' "1" "$rc"
check 'and the message names the file' "1" \
	"$(said "$out" 'file=.github/workflows/tests.yml')"
check 'and the message names the line the pin sits on' "1" \
	"$(said "$out" 'line 7:')"
check 'and the message names the pin itself' "1" "$(said "$out" "$BAD_PIN")"
check 'and the message names the length it found' "1" \
	"$(said "$out" 'which is 39 characters')"
check 'and the message says what the length must be' "1" \
	"$(said "$out" 'exactly 40 hexadecimal')"
check 'and the message says why it matters: the job never runs' "1" \
	"$(said "$out" 'the job never runs')"

# --- the paired acceptance: the same shape with the correct pin ----------
d="$(new)"; pinned "$d" "$GOOD_PIN"
rc=0; run_in "$d" "$WORK/pin-length.sh" >/dev/null || rc=$?
check 'the same workflow with a 40-character pin is accepted' "0" "$rc"
out="$(run_in "$d" "$WORK/pin-length.sh")"
check 'and the step says every pin is 40 characters' "1" \
	"$(said "$out" 'exactly 40 hexadecimal')"

# ===========================================================================
# Length on the other side, and the non-hex case. Both are rejected by
# GitHub for the same reason, so both are refused here.
# ===========================================================================

d="$(new)"; pinned "$d" "${GOOD_PIN}f"
rc=0; run_in "$d" "$WORK/pin-length.sh" >/dev/null || rc=$?
check 'a 41-character pin is refused too' "1" "$rc"

d="$(new)"; pinned "$d" "${GOOD_PIN:0:39}z"
out="$(run_in "$d" "$WORK/pin-length.sh")"; rc=$?
check 'a 40-character pin with a non-hex character is refused' "1" "$rc"
check 'and the refusal is about hexadecimal, not just length' "1" \
	"$(said "$out" 'hexadecimal')"

# ===========================================================================
# Shapes with no pin to measure. Refusing these would take the
# repository down, since every workflow here calls a local reusable.
# ===========================================================================

# --- a local reusable call carries no pin -------------------------------
d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  shellcheck:
    uses: ./.github/workflows/reusable-shell-ci.yml
    with:
      test-command: bash ./tests/shell/locale-pinned.test.sh
EOF
rc=0; run_in "$d" "$WORK/pin-length.sh" >/dev/null || rc=$?
check 'a local reusable call is not in scope' "0" "$rc"

# --- a tag reference is refused: a tag is not a pin ---------------------
# `uses: owner/repo@v7` is a moving reference, not a pin, and no other
# control in this repository refuses it. Nothing in the tree uses the
# form today, so measuring it here costs nothing and closes the gap
# rather than leaving it for a second check that does not exist.
d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
EOF
out="$(run_in "$d" "$WORK/pin-length.sh")"; rc=$?
check 'a tag reference is refused: a tag is not a 40-character pin' "1" "$rc"
check 'and the message names the tag it found' "1" "$(said "$out" '`v7`')"

# --- a commented-out pin is prose, not wiring ---------------------------
# reusable-dependabot-freshness.yml carries `uses: owner/repo@<sha>` in a
# comment describing its own grep. A check that read comments would fail
# the tree over documentation, which is the defect
# ci-runs-every-test.test.sh already records about its first version.
d="$(new)"; pinned "$d" "$GOOD_PIN"
cat >> "$d/.github/workflows/tests.yml" <<EOF
      # - uses: actions/checkout@$BAD_PIN # v7.0.1
EOF
rc=0; run_in "$d" "$WORK/pin-length.sh" >/dev/null || rc=$?
check 'a commented-out short pin is not a violation' "0" "$rc"

# ===========================================================================
# Reach: the assertion must see every workflow file, and every pin
# within one. A check that stopped at the first file, or the first pin
# in a file, would have passed the tree of #1741 whenever the bad pin
# was not the first one it met. It was the sixth.
# ===========================================================================

d="$(new)"; pinned "$d" "$GOOD_PIN"
cat > "$d/.github/workflows/second.yml" <<EOF
name: Second
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@$BAD_PIN # v7.0.1
EOF
out="$(run_in "$d" "$WORK/pin-length.sh")"; rc=$?
check 'a bad pin in the second workflow file is still found' "1" "$rc"
check 'and the message names that file, not the first one' "1" \
	"$(said "$out" 'file=.github/workflows/second.yml')"

d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@$GOOD_PIN # v7.0.1
      - uses: actions/setup-python@$GOOD_PIN # v6.0.0
      - uses: actions/upload-artifact@$BAD_PIN # v7.0.1
EOF
out="$(run_in "$d" "$WORK/pin-length.sh")"; rc=$?
check 'the third pin in a file is reached, not just the first' "1" "$rc"
check 'and the message names the offending action, not a sibling' "1" \
	"$(said "$out" 'actions/upload-artifact')"
check 'and the good siblings are not reported' "0" \
	"$(said "$out" 'actions/setup-python')"

# --- the `.yaml` spelling is a workflow too -----------------------------
d="$(new)"
cat > "$d/.github/workflows/tests.yaml" <<EOF
name: Tests
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@$BAD_PIN # v7.0.1
EOF
rc=0; run_in "$d" "$WORK/pin-length.sh" >/dev/null || rc=$?
check 'a bad pin in a .yaml workflow is found too' "1" "$rc"

# ===========================================================================
# Edge: the non-list `uses:` form. A reusable caller writes `uses:` as
# a job property with no leading dash, and a third-party action can be
# written the same way under `steps:` when other keys precede it. Both
# spellings carry a pin, so both are measured.
# ===========================================================================

d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Check out
        uses: actions/checkout@$BAD_PIN # v7.0.1
EOF
out="$(run_in "$d" "$WORK/pin-length.sh")"; rc=$?
check 'the named-step spelling of uses: is measured too' "1" "$rc"
check 'and it names the line the uses: sits on, not the name:' "1" \
	"$(said "$out" 'line 8:')"

d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - name: Check out
        uses: actions/checkout@$GOOD_PIN # v7.0.1
EOF
rc=0; run_in "$d" "$WORK/pin-length.sh" >/dev/null || rc=$?
check 'and the same spelling with a good pin is accepted' "0" "$rc"

# ===========================================================================
# Edge: an empty workflow directory is not a violation. A check that
# treated "no pins found" as a failure would go red on a repository
# that had not written a workflow yet, which is noise rather than a
# finding.
# ===========================================================================

d="$(new)"
rc=0; run_in "$d" "$WORK/pin-length.sh" >/dev/null || rc=$?
check 'a workflow directory with no files passes' "0" "$rc"

# ===========================================================================
# Edge: an unreadable workflow is a warning, not a silent skip. Same
# shape as tooling-isolation and caller-if: the silence stays visible.
# ===========================================================================

d="$(new)"; pinned "$d" "$GOOD_PIN"
printf 'name: Unreadable\n' > "$d/.github/workflows/locked.yml"
chmod 000 "$d/.github/workflows/locked.yml"
out="$(run_in "$d" "$WORK/pin-length.sh")"; rc=$?
chmod 644 "$d/.github/workflows/locked.yml"
# Running as root defeats the permission bit, so the warning only has
# to appear where the read actually fails. The step must not fail
# either way, which is the property under test.
check 'an unreadable workflow does not fail the step' "0" "$rc"
if [ "$(id -u)" -ne 0 ]; then
	check 'but is reported as skipped' "1" "$(said "$out" '::warning')"
else
	echo "skip  but is reported as skipped (running as root)"
fi

echo
echo "$pass passed, $fail failed"
printf 'DONE %s %d %d\n' "${BASH_SOURCE[0]##*/}" "$pass" "$fail"
[ "$fail" -eq 0 ]
