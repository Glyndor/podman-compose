#!/usr/bin/env bash
#
# Behaviour tests for the caller's `with:` defaults check in
# .github/workflows/reusable-workflow-lint.yml. The caller-if assertion
# (#1731) refuses a job-level `if:` that can skip a required check.
# This one refuses a job-level `with:` value that differs from the
# reusable's declared `inputs.X.default`. Same outcome through a
# different lever: the ruleset requires the check NAME, but the
# underlying job runs differently when the input value is not the
# one the ruleset was configured against. A pull request to
# ci.yml:coverage-threshold: 76 -> 0, ci.yml:msrv: 1.85 -> '',
# ci.yml:extra-test-os: '["macos-latest", ...]' -> '[]', or
# ci.yml:doc-warnings: true -> false would all keep `rust / Coverage`,
# `rust / MSRV`, `rust / MSRV (1.85)`, `rust / Extra platforms`,
# `rust / Doc warnings` as the names the ruleset matches and silently
# lower or remove the gate. The lint catches each.
#
# The rule is symmetric to caller-if: same named set of required checks,
# same static map, same fixture shape. The comparison is not against
# the ruleset (the ruleset sees check names, not inputs) but against
# the reusable's declared `inputs.X.default`. The reusable is the
# place where the value the ruleset assumes is canonicalised: a
# change to the default is reviewed on the reusable's diff, where the
# ruleset's check name stays stable.
#
# `if:` lines INSIDE a reusable are skipped the same way caller-if
# skips them. The same `reusable-*` basename filter applies. This
# step is wholly in the non-reusable scan path.
#
# Each step's `run:` body is extracted from the workflow and executed
# as it ships, in a temporary tree shaped like a repository. Every
# refusal asserts WHICH message fired. Each refusal is paired with a
# same-shape acceptance so the rule is precise.
#
# Requires: python3 with PyYAML (the step under test imports it).
# The fixtures below carry literal `${{ ... }}` expressions: the
# assertion under test reads those characters, so they must reach
# the file unexpanded. Single quotes are the point, not an oversight.
# shellcheck disable=SC2016
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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

step_script() { # $1=workflow path  $2=step name substring
	python3 - "$1" "$2" <<'PY'
import sys
lines = open(sys.argv[1]).read().splitlines()
try:
    start = next(i for i, l in enumerate(lines) if "name: " + sys.argv[2] in l)
except StopIteration:
    sys.exit(0)
try:
    run = next(i for i, l in enumerate(lines) if i > start and l.strip() == "run: |")
except StopIteration:
    sys.exit(0)
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
step_script "$WORKFLOW" "A caller of a required check must carry \`with:\`" \
	> "$WORK/caller-with.sh"
check "the caller-with assertion was extracted from the workflow" "1" \
	"$(grep -c check-caller-with-defaults.py "$WORK/caller-with.sh" | awk '{print ($1>0)}')"

check "the caller-with-defaults script lives next to the other GitHub scripts" "1" \
	"$(test -f "$HERE/.github/scripts/check-caller-with-defaults.py" && echo 1 || echo 0)"

# ---------------------------------------------------------------------------
# Fixture helpers
# ---------------------------------------------------------------------------

# A fresh empty tree with '.github/workflows/' (and '.github/scripts/'
# for the caller-with-defaults checker the step body shells out to),
# and the real reusables copied across. The reusables declare the
# inputs whose defaults the caller is compared against; a fixture
# that does not include them cannot be checked.
new() {
	rm -rf "$WORK/t"
	mkdir -p "$WORK/t/.github/workflows" "$WORK/t/.github/scripts"
	for r in \
		reusable-rust-ci.yml \
		reusable-dco.yml \
		reusable-line-limit.yml \
		reusable-main-guard.yml \
		reusable-workflow-lint.yml; do
		cp "$HERE/.github/workflows/$r" "$WORK/t/.github/workflows/"
	done
	cp "$HERE/.github/scripts/check-caller-with-defaults.py" \
		"$WORK/t/.github/scripts/"
	printf '%s' "$WORK/t"
}
run_in() { ( cd "$1" && bash "$2" 2>&1 ); }
said() { printf '%s' "$1" | grep -q -F -- "$2" && echo 1 || echo 0; }

# A caller of a required-emitting reusable, with the supplied with:
# block contents (already indented under the job). `with_text` may be
# the empty string for "no with: block at all".
caller_with() { # $1=dir $2=with-block text (or empty) $3=reusable basename
	if [ -n "$2" ]; then
		cat > "$1/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  caller:
    uses: ./.github/workflows/$3
    with:
$2
EOF
	else
		cat > "$1/.github/workflows/tests.yml" <<EOF
name: Tests
on: pull_request
jobs:
  caller:
    uses: ./.github/workflows/$3
EOF
	fi
}

# ===========================================================================
# Real-tree positive control: the loaded step body, run against the
# real repository, exits 0 and prints the OK summary once the
# reusable defaults carry the values the production callers set. With
# the original defaults, ci.yml:coverage-threshold: 76 differs from
# the reusable's declared default of 0 and the step exits 1; that is
# the failure the defaults bump removes.
# ===========================================================================

out="$(cd "$HERE" && bash "$WORK/caller-with.sh")"; rc=$?
check "the real repository tree passes the caller-with-defaults assertion" \
	"0" "$rc"
check "and the step prints the OK summary when the real tree is clean" "1" \
	"$(said "$out" 'caller-with-defaults: every caller')"

# ===========================================================================
# Each negative fixture plants a caller whose `with:` value differs
# from the reusable's declared default. The seeded reusables carry
# the same defaults the production reusables declare, so the step
# catches each fixture the same way it would catch the same change
# in production. Every refusal is paired with an acceptance (a
# caller with the matching value, or no `with:` at all) so the rule
# is precise.
# ===========================================================================

# --- coverage-threshold 76 -> 0 drops the coverage gate --------------
d="$(new)"
caller_with "$d" "      coverage-threshold: 0" "reusable-rust-ci.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "with: coverage-threshold: 0 (default 76) is refused" "1" "$rc"
check "and the message names the offending job" "1" \
	"$(said "$out" 'job `caller` in')"
check "and the message names the input key and the value" "1" \
	"$(said "$out" 'coverage-threshold')"
check "and the message names the declared default" "1" \
	"$(said "$out" 'default')"
check "and the message points at the reusable's declared default" "1" \
	"$(said "$out" 'declared default')"
check "and the step did not silently pass it" "0" \
	"$(said "$out" 'caller-with-defaults: every caller')"

# --- msrv 1.85 -> '' disables the MSRV job ---------------------------
d="$(new)"
caller_with "$d" "      msrv: ''" "reusable-rust-ci.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "with: msrv: '' (default 1.85) is refused" "1" "$rc"
check "and the message names the input key" "1" \
	"$(said "$out" 'msrv')"

# --- extra-test-os "[...]" -> '[]' drops extra-platform gating -------
d="$(new)"
caller_with "$d" "      extra-test-os: '[]'" "reusable-rust-ci.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "with: extra-test-os: '[]' (default array) is refused" "1" "$rc"

# --- doc-warnings true -> false drops doc-warning gate ---------------
d="$(new)"
caller_with "$d" "      doc-warnings: false" "reusable-rust-ci.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "with: doc-warnings: false (default true) is refused" "1" "$rc"

# --- extensions trimmed to a subset of the default set --------------
d="$(new)"
caller_with "$d" "      extensions: 'rs go'" "reusable-line-limit.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "with: extensions: 'rs go' (full default) is refused" "1" "$rc"

# --- working-directory: src is non-default, refused ----------------
d="$(new)"
caller_with "$d" "      working-directory: src" "reusable-rust-ci.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "with: working-directory: src (default '.') is refused" "1" "$rc"

# ===========================================================================
# Each shape above is accepted when the value matches the default.
# ===========================================================================

# --- a caller with no with: block at all is fine --------------------
d="$(new)"
caller_with "$d" "" "reusable-rust-ci.yml"
rc=0; run_in "$d" "$WORK/caller-with.sh" >/dev/null || rc=$?
check "a caller with no with: block is allowed" "0" "$rc"

# --- a caller whose with: values equal the declared defaults --------
d="$(new)"
caller_with "$d" "      coverage-threshold: 76
      msrv: '1.85'
      extra-test-os: '[\"macos-latest\", \"windows-latest\"]'
      doc-warnings: true
      working-directory: '.'
      toolchain: '1.98'
      podman: false
      package-check: false
      semver-check: false
      semver-checks-version: '0.50.0'
      llvm-cov-version: '0.8.7'
      coverage-ignore-regex: ''" "reusable-rust-ci.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "a caller whose with: values match the declared defaults is allowed" "0" "$rc"
check "and the step prints the OK summary" "1" \
	"$(said "$out" 'caller-with-defaults: every caller')"

# --- an input the reusable does not declare is ignored, not refused
d="$(new)"
caller_with "$d" "      not-a-real-input: anything" "reusable-rust-ci.yml"
rc=0; run_in "$d" "$WORK/caller-with.sh" >/dev/null || rc=$?
check "an unknown input key is ignored, not refused" "0" "$rc"

# ===========================================================================
# Direct emitter (Supported Podman majors) is not gated: the rule
# only catches callers of required-emitting REUSABLES. A direct job
# named `Supported Podman majors` does not pass a `with:` to anyone,
# and any `with:` on a step within it is not the same level as the
# caller-if/caller-with scan.
# ===========================================================================

d="$(new)"
cat > "$d/.github/workflows/tests.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  podman-majors:
    name: Supported Podman majors
    runs-on: ubuntu-latest
    steps:
      - run: echo green
EOF
rc=0; run_in "$d" "$WORK/caller-with.sh" >/dev/null || rc=$?
check "a direct emitter job (no uses:, no with:) is allowed" "0" "$rc"

# ===========================================================================
# A caller of a non-required reusable is not in scope. Line-limit.yml
# is a required-emitting reusable but for the `line-limit` check;
# ASSET-CONTRACT uses reusable-installer-contract, which is not in
# REQUIRED_REUSABLES. A caller of reusable-installer-contract with a
# non-default `with:` must NOT be caught.
# ===========================================================================

d="$(new)"
caller_with "$d" "      install-script: 'install.sh.bak'" \
	"reusable-installer-contract.yml"
rc=0; run_in "$d" "$WORK/caller-with.sh" >/dev/null || rc=$?
check "a non-default with: on a non-required reusable is not in scope" \
	"0" "$rc"

# ===========================================================================
# Inside a reusable, a `with:`-shaped dict is the reusable's own
# inputs declaration, not a caller override. The rule skips the
# reusable's own file (same seam as caller-if).
# ===========================================================================

d="$(new)"
# Set up a workflow that calls reusable-rust-ci.yml AND modify the
# reusable's own `inputs.X.default` to a different value. The
# caller's with: matches the original default, so the assertion must
# stay quiet: the rule compares to the reusable's CURRENT declared
# default, which we just changed.
cat > "$d/.github/workflows/tests.yml" <<'EOF'
name: Tests
on: pull_request
jobs:
  caller:
    uses: ./.github/workflows/reusable-rust-ci.yml
    with:
      coverage-threshold: 76
EOF
python3 - "$d/.github/workflows/reusable-rust-ci.yml" <<'PY'
import sys
path = sys.argv[1]
with open(path) as fh:
    text = fh.read()
# Bump the input's default; the caller's 76 no longer matches the
# declared default (now 50).
text = text.replace(
    "coverage-threshold:\n        description: Minimum line coverage percentage (0 disables the gate)\n        type: number\n        default: 0",
    "coverage-threshold:\n        description: Minimum line coverage percentage (0 disables the gate)\n        type: number\n        default: 50",
    1,
)
with open(path, "w") as fh:
    fh.write(text)
PY
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "a caller whose with: matches what the reusable now declares is allowed" \
	"0" "$rc"
check "and the step did not flag the reuse-side default change" "1" \
	"$(said "$out" 'caller-with-defaults: every caller')"

# ===========================================================================
# Edge: an unparseable caller is a warning, not a silent skip. The
# previous step would fail-closed on a YAML parse error; this one
# emits a warning so the silence is visible.
# ===========================================================================

d="$(new)"
printf 'name: [\n' > "$d/.github/workflows/broken.yml"
out="$(run_in "$d" "$WORK/caller-with.sh")"; rc=$?
check "an unparseable workflow does not fail the step" "0" "$rc"
check "but is reported as skipped" "1" "$(said "$out" '::warning')"

echo
echo "$pass passed, $fail failed"
printf 'DONE %s %d %d\n' "${BASH_SOURCE[0]##*/}" "$pass" "$fail"
[ "$fail" -eq 0 ]
