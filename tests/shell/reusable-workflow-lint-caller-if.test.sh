#!/usr/bin/env bash
#
# Behaviour tests for the caller-if assertion in
# .github/workflows/reusable-workflow-lint.yml: a caller of a job that
# emits a required check name must not carry a switchable job-level
# 'if:'.
#
# Eleven status checks are required on 'main' and ten on 'develop'.
# Every one of them is emitted either directly by a job in this tree or
# by a reusable a job here calls. GitHub reports a SKIPPED required
# check as Success, so a job-level 'if: false' (or anything else that
# can evaluate false) switches the gate off without failing it. The
# assertion refuses that, with one message naming the file, the job,
# and the property being violated.
#
# 'if: always()' is the one allowed form. It can never evaluate false,
# so the job always runs and the check is always reported. This is the
# load-bearing pattern from #1552: a gate job summarises a matrix that
# may fail, so it has to run on 'failure' too and reads 'needs.*.result'
# to make its verdict. The podman-lane gate and the rust-ci gates are
# the documented cases in this repository, and the assertion keeps them
# passing while refusing every other value.
#
# 'if:' lines INSIDE a reusable are not in scope: they configure how
# the reusable behaves ('if: inputs.msrv !=') and refusing them would
# take down the repository by deleting controls. The assertion skips
# files whose basename starts with 'reusable-', so it never reaches
# them. One of the tests plants a switching 'if:' inside
# reusable-rust-ci.yml-shaped content to prove the seam holds.
#
# Each step's 'run:' body is extracted from the workflow and executed
# as it ships, in a temporary tree shaped like a repository. Every
# refusal asserts WHICH message fired. Each refusal is paired with an
# acceptance of the same shape just inside the line.
#
# Requires: python3 with PyYAML (the step under test imports it).
# The fixtures below carry literal '${{ ... }}' expressions and
# backticks: the assertion under test reads those characters, so they
# must reach the file unexpanded. Single quotes are the point, not an
# oversight.
# shellcheck disable=SC2016
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0

# All test descriptions are passed via this helper as single-quoted
# strings. Backticks inside double quotes would be command substitution
# in bash, so the helper takes its argument unquoted and the call sites
# use single quotes throughout.
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

# Pull one step's 'run:' body out of a workflow, dedented, so it can be
# run. Same helper as the other tests in this directory: the point is
# to exercise the script as it ships rather than a copy of it that can
# drift.
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
step_script "$WORKFLOW" "A caller of a required check" > "$WORK/caller-if.sh"
check 'the caller-if assertion was extracted from the workflow' "1" \
	"$(grep -c 'REQUIRED_REUSABLES' "$WORK/caller-if.sh" | awk '{print ($1>0)}')"

# ---------------------------------------------------------------------------
# Fixture helpers
# ---------------------------------------------------------------------------

# A fresh empty tree with '.github/workflows/', and the real reusables
# copied across. The reusables are needed because the assertion walks
# them to learn which required check names they emit; without them the
# caller scan has nothing to look at.
new() {
	rm -rf "$WORK/t"
	mkdir -p "$WORK/t/.github/workflows"
	for r in \
		reusable-rust-ci.yml \
		reusable-dco.yml \
		reusable-line-limit.yml \
		reusable-main-guard.yml \
		reusable-workflow-lint.yml; do
		cp "$HERE/.github/workflows/$r" "$WORK/t/.github/workflows/"
	done
	printf '%s' "$WORK/t"
}
run_in() { # $1=dir $2=script -> stdout+stderr, status in $?
	( cd "$1" && bash "$2" 2>&1 )
}
said() { printf '%s' "$1" | grep -q -F -- "$2" && echo 1 || echo 0; }

# A caller of a required-emitting reusable with a placeholder if-value
# or no 'if:' at all (when $2 is empty). The reusable referenced here
# is one whose required names are listed in REQUIRED_REUSABLES; any of
# the five will do for the assertions.
caller() { # $1=dir $2=if-value (or empty) $3=reusable basename
	if [ -n "$2" ]; then
		ifline="    if: $2"
	else
		ifline=""
	fi
	cat > "$1/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  caller:
$ifline
    uses: ./.github/workflows/$3
EOF
}

# A direct-emitting job: 'name: Supported Podman majors' on the
# 'podman-majors' job, matching DIRECT_EMITTER_NAMES. The optional
# 'if:' value sits between the 'name:' and 'runs-on:'.
direct() { # $1=dir $2=if-value (or empty)
	if [ -n "$2" ]; then
		ifline="    if: $2"
	else
		ifline=""
	fi
	cat > "$1/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  podman-majors:
    name: Supported Podman majors
$ifline
    runs-on: ubuntu-latest
    steps:
      - run: echo green
EOF
}

# ===========================================================================
# Positive control: the real tree passes (the load-bearing if: always()
# on the podman-lane gate stays allowed).
# ===========================================================================

out="$(cd "$HERE" && bash "$WORK/caller-if.sh")"; rc=$?
check 'the real repository tree passes the caller-if assertion' "0" "$rc"
check 'and the step prints the OK summary, not a refusal' "1" \
	"$(said "$out" 'caller-if: every caller of a required check')"

# ===========================================================================
# Negative fixtures: each refusal is paired with a same-shape positive
# control so the acceptance is not implicit.
# ===========================================================================

# --- a job-level if: false on a caller is refused ------------------------
d="$(new)"; caller "$d" 'false' 'reusable-rust-ci.yml'
out="$(run_in "$d" "$WORK/caller-if.sh")"; rc=$?
check 'a job-level if: false on a reusable caller is refused' "1" "$rc"
check 'and the message names the offending job' "1" \
	"$(said "$out" 'job `caller` in')"
check 'and the message names the reusable required checks' "1" \
	"$(said "$out" 'required checks')"
check 'and the message names the value as a switch, not a filter' "1" \
	"$(said "$out" 'switch, not a filter')"
check 'and the message points at always() as the blessed form' "1" \
	"$(said "$out" 'Only `if: always()` is allowed')"

# --- a job-level if: ${{ ... }} over github.event_name is refused --------
d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  caller:
    if: ${{ github.event_name == 'pull_request' }}
    uses: ./.github/workflows/reusable-rust-ci.yml
EOF
out="$(run_in "$d" "$WORK/caller-if.sh")"; rc=$?
check 'a switching if: over github.event_name is refused' "1" "$rc"
check 'and the offending job is named' "1" "$(said "$out" '`caller`')"

# --- a job-level if: inputs.X on a reusable caller is refused ------------
d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  caller:
    if: inputs.coverage-threshold > 0
    uses: ./.github/workflows/reusable-rust-ci.yml
EOF
out="$(run_in "$d" "$WORK/caller-if.sh")"; rc=$?
check 'an if: inputs.X on a reusable caller is refused' "1" "$rc"

# --- a direct-emitting job (podman-majors) with if: false is refused ----
d="$(new)"; direct "$d" 'false'
out="$(run_in "$d" "$WORK/caller-if.sh")"; rc=$?
check 'an if: false on the direct podman-majors job is refused' "1" "$rc"
check 'and the message names the required check, not just the job' "1" \
	"$(said "$out" 'required check `Supported Podman majors`')"

# --- a direct-emitting job with if: success() is refused -----------------
d="$(new)"; direct "$d" 'success()'
out="$(run_in "$d" "$WORK/caller-if.sh")"; rc=$?
check 'an if: success() on a direct emitter is refused' "1" "$rc"

# ===========================================================================
# Positive controls inside the negative fixtures: each shape above is
# accepted when the conditional is removed or replaced with always().
# ===========================================================================

# --- a caller with no if: at all is allowed ------------------------------
d="$(new)"; caller "$d" '' 'reusable-rust-ci.yml'
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'a caller with no job-level if: is allowed' "0" "$rc"

# --- a caller with if: always() is the blessed form ---------------------
d="$(new)"; caller "$d" 'always()' 'reusable-rust-ci.yml'
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'an if: always() on a reusable caller is allowed' "0" "$rc"
out="$(run_in "$d" "$WORK/caller-if.sh")"
check 'and the step says every caller passes' "1" \
	"$(said "$out" 'every caller of a required check')"

# --- the wrapped spelling of always() means the same thing ---------------
# A bare if: is evaluated as an expression either way, so refusing the
# wrapped form would refuse valid code. Its tail-carrying cousin can still
# evaluate false and stays refused, which is what makes this a real seam
# rather than a substring match.
d="$(new)"; caller "$d" '${{ always() }}' 'reusable-rust-ci.yml'
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'the wrapped spelling of always() is allowed too' "0" "$rc"

d="$(new)"; caller "$d" 'always() && needs.build.result == "success"' 'reusable-rust-ci.yml'
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'always() with a tail can still be false, so it is refused' "1" "$rc"

# --- a direct-emitting job with if: always() is allowed -----------------
d="$(new)"; direct "$d" 'always()'
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'an if: always() on the direct podman-majors job is allowed' "0" "$rc"

# --- a direct-emitting job with no if: is allowed ------------------------
d="$(new)"; direct "$d" ''
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'the direct podman-majors job with no if: is allowed' "0" "$rc"

# ===========================================================================
# Inside a reusable, if: configs the reusable. Refusing it would break
# the whole repository. The assertion skips reusable-* files, so a
# switching 'if:' inside reusable-rust-ci.yml stays accepted.
# ===========================================================================

d="$(new)"
cat > "$d/.github/workflows/some-job.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  caller:
    uses: ./.github/workflows/reusable-rust-ci.yml
EOF
# Replace the test-extra job's 'if: inputs.extra-test-os != ...' with a
# switching expression that would be refused on a caller. The reusable's
# own job must stay accepted, because refusing it would take the
# repository down.
python3 - "$d/.github/workflows/reusable-rust-ci.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
text = text.replace(
    "  test-extra:\n    name: Test (${{ matrix.os }})\n    if: inputs.extra-test-os != '[]'",
    "  test-extra:\n    name: Test (${{ matrix.os }})\n    if: ${{ github.event_name == 'pull_request' }}",
    1,
)
with open(path, "w") as fh:
    fh.write(text)
PY
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'a switching if: inside a reusable is still allowed' "0" "$rc"
out="$(run_in "$d" "$WORK/caller-if.sh")"
check 'and the step did not flag the reusable own job' "0" \
	"$(said "$out" 'test-extra')"
check 'and the step says every caller passes' "1" \
	"$(said "$out" 'every caller of a required check')"

# ===========================================================================
# Edge: a non-required job carrying if: is not in scope. The assertion
# only fires on jobs that emit a required check, so an if: on a
# different job stays allowed. This is the seam that keeps the friction
# local to the gate.
# ===========================================================================

d="$(new)"
cat > "$d/.github/workflows/some-job.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  unrelated:
    if: github.event_name == 'pull_request'
    runs-on: ubuntu-latest
    steps:
      - run: echo green
EOF
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'an if: on a non-required job is allowed' "0" "$rc"

# ===========================================================================
# Edge: a caller of a reusable that is NOT in REQUIRED_REUSABLES. The
# callers of reusables not on the list are not gated, because the
# assertion cannot prove their jobs are required.
# ===========================================================================

d="$(new)"
cat > "$d/.github/workflows/some-job.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  caller:
    if: false
    uses: ./.github/workflows/reusable-shell-ci.yml
EOF
rc=0; run_in "$d" "$WORK/caller-if.sh" >/dev/null || rc=$?
check 'an if: false on a non-required reusable caller is allowed' "0" "$rc"

# ===========================================================================
# Edge: an unparseable caller is a warning, not a silent skip. The
# previous form of a check here would fail-closed on a YAML parse
# error; this form follows tooling-isolation and reports it as a
# ::warning so the silence is visible.
# ===========================================================================

d="$(new)"; printf 'name: [\n' > "$d/.github/workflows/broken.yml"
out="$(run_in "$d" "$WORK/caller-if.sh")"; rc=$?
check 'an unparseable workflow does not fail the step' "0" "$rc"
check 'but is reported as skipped' "1" "$(said "$out" '::warning')"

echo
echo "$pass passed, $fail failed"
printf 'DONE %s %d %d\n' "${BASH_SOURCE[0]##*/}" "$pass" "$fail"
[ "$fail" -eq 0 ]
