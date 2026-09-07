#!/usr/bin/env bash
#
# Behaviour tests for the tooling-isolation assertion in
# .github/workflows/reusable-workflow-lint.yml: a job that holds a secret
# must not install third-party tooling.
#
# The step parses every workflow under .github/workflows and refuses, with
# its own message, a job that calls cargo/go/gem/npm install or `pip
# install` (without --require-hashes) while holding a secret reference. The
# step is the gate; a script that gates gets tests, and this is the test.
#
# Two shapes of secret reach a step and were not seen before #1720:
#
#   1. The bracket-index form, `secrets['NAME']` or `secrets["NAME"]`,
#      with optional whitespace inside the brackets. The reference shape
#      `secrets.NAME` was the only one the previous regex matched.
#
#   2. A workflow-level `env:`. Every job in the workflow inherits it, so
#      a secret declared there reaches every step. The scan reserialised
#      the job, not the workflow-level env, so it never looked at it.
#
# Each refusal asserts WHICH message fired. Naming where the secret comes
# from matters when it is not in the job: an author reading "job X holds a
# secret" and seeing no secret in job X concludes the check is wrong rather
# than looking further up the file.
#
# The `secrets: inherit` line and the `secrets:` declaration block on a
# reusable call stay unrefused: those are declaration blocks, not
# references, and `secrets:` is not followed by `.` or `[`.
#
# Each step's `run:` body is extracted from the workflow and executed as it
# ships, in a temporary tree shaped like a repository. Every refusal is
# paired with an acceptance of the same shape just inside the line.
#
# Requires: python3 with PyYAML (the tooling-isolation step imports it).
# The fixtures below carry literal `${{ secrets.X }}` expressions and
# backticks: the assertion under test reads those characters, so they must
# reach the file unexpanded. Single quotes are the point, not an oversight.
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

# Pull one step's `run:` body out of a workflow, dedented, so it can be run.
# Same helper shape as tests/shell/ci-runs-every-test.test.sh: the point
# is to exercise the script as it ships rather than a copy of it that can
# drift.
step_script() { # $1=workflow path  $2=step name substring
	python3 - "$1" "$2" <<'PY'
import sys
lines = open(sys.argv[1]).read().splitlines()
start = next(i for i, l in enumerate(lines) if "name: " + sys.argv[2] in l)
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
step_script "$WORKFLOW" "A job holding a secret must not install" > "$WORK/tooling.sh"
check "the tooling assertion was extracted from the workflow" "1" \
	"$(grep -c 'INSTALL_PATTERNS' "$WORK/tooling.sh" | awk '{print ($1>0)}')"

new() { rm -rf "$WORK/t"; mkdir -p "$WORK/t/.github/workflows"; printf '%s' "$WORK/t"; }
run_in() { # $1=dir $2=script -> stdout+stderr, status in $?
	( cd "$1" && bash "$2" 2>&1 )
}
said() { printf '%s' "$1" | grep -q -- "$2" && echo 1 || echo 0; }

tooling_case() { # $1=dir $2=env line (or empty) $3=run line
	cat > "$1/.github/workflows/job.yml" <<EOF
name: Job
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      $2
    steps:
      - run: |
          $3
EOF
}

# ===========================================================================
# A job holding a secret must not install third-party tooling
# ===========================================================================

# --- a secret plus a pip install is refused, and named --------------------
d="$(new)"; tooling_case "$d" 'TOKEN: ${{ secrets.DEPLOY_TOKEN }}' 'pip install requests'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "a job holding a secret that pip installs is refused" "1" "$rc"
check "and the message names the job and the install line" "1" \
	"$(said "$out" 'job `build` installs third-party tooling while holding a secret from this job: `pip install requests`')"

# --- the same install pinned by hash is the documented exemption ----------
d="$(new)"; tooling_case "$d" 'TOKEN: ${{ secrets.DEPLOY_TOKEN }}' 'pip install --require-hashes -r req.txt'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "pip install --require-hashes while holding a secret is allowed" "0" "$rc"
check "and the step says nothing was found" "1" "$(said "$out" 'no job holding a secret installs')"

# --- cargo install has no hash exemption ---------------------------------
d="$(new)"; tooling_case "$d" 'TOKEN: ${{ secrets.DEPLOY_TOKEN }}' 'cargo install --locked cargo-cyclonedx'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "cargo install --locked while holding a secret is still refused" "1" "$rc"

# --- the same install without a secret is fine ---------------------------
d="$(new)"; tooling_case "$d" 'PLAIN: value' 'cargo install --locked cargo-cyclonedx'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "the same install in a job holding no secret passes" "0" "$rc"

# --- a commented-out install is not an install ---------------------------
d="$(new)"; tooling_case "$d" 'TOKEN: ${{ secrets.DEPLOY_TOKEN }}' '# pip install requests'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "an install that only appears in a comment is not reported" "0" "$rc"

# --- a secret referenced in a step, not the job env, still counts --------
d="$(new)"
cat > "$d/.github/workflows/job.yml" <<'EOF'
name: Job
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: go install example.com/tool@v1
      - run: echo "$KEY"
        env:
          KEY: ${{ secrets.SIGNING_KEY }}
EOF
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "a secret held by a later step of the same job still counts" "1" "$rc"

# --- a `secrets: inherit` declaration is not a reference -----------------
d="$(new)"
cat > "$d/.github/workflows/job.yml" <<'EOF'
name: Job
on: push
jobs:
  call:
    uses: ./.github/workflows/reusable-x.yml
    secrets: inherit
  build:
    runs-on: ubuntu-latest
    steps:
      - run: go install example.com/tool@v1
EOF
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "secrets: inherit on a reusable call is not a secret reference" "0" "$rc"

# --- the bracket-index form secrets['X'] is caught, single-quoted --------
d="$(new)"
cat > "$d/.github/workflows/job.yml" <<'EOF'
name: Job
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      TOKEN: ${{ secrets['DEPLOY_TOKEN'] }}
    steps:
      - run: |
          pip install requests
EOF
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "a job holding a secret via secrets['X'] is refused" "1" "$rc"
check "and the message names the job and the install line" "1" \
	"$(said "$out" 'job `build` installs third-party tooling while holding a secret from this job: `pip install requests`')"
check "and the step did not silently pass it" "0" \
	"$(said "$out" 'no job holding a secret installs')"
check "and there is no parse-error warning hiding the miss" "0" \
	"$(said "$out" '::warning')"

# --- the same bracket-index form, double-quoted, is also caught ----------
d="$(new)"
cat > "$d/.github/workflows/job.yml" <<'EOF'
name: Job
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      TOKEN: ${{ secrets["DEPLOY_TOKEN"] }}
    steps:
      - run: |
          pip install requests
EOF
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "a job holding a secret via secrets[\"X\"] is refused" "1" "$rc"
check "and the message names the job and the install line" "1" \
	"$(said "$out" 'job `build` installs third-party tooling while holding a secret from this job: `pip install requests`')"
check "and the step did not silently pass it" "0" \
	"$(said "$out" 'no job holding a secret installs')"

# --- bracket-index form with whitespace inside the brackets is caught -----
d="$(new)"
cat > "$d/.github/workflows/job.yml" <<'EOF'
name: Job
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      TOKEN: ${{ secrets[ 'DEPLOY_TOKEN' ] }}
    steps:
      - run: |
          pip install requests
EOF
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "a job holding a secret via secrets[ 'X' ] (whitespace) is refused" "1" "$rc"
check "and the message names the job and the install line" "1" \
	"$(said "$out" 'job `build` installs third-party tooling while holding a secret from this job: `pip install requests`')"

# --- a workflow-level env that holds a secret catches every job -----------
d="$(new)"
cat > "$d/.github/workflows/job.yml" <<'EOF'
name: Job
on: push
env:
  TOKEN: ${{ secrets.DEPLOY_TOKEN }}
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: |
          pip install requests
EOF
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "a job whose workflow-level env holds a secret is refused" "1" "$rc"
check "and the message names workflow-level env as the source, not the job" "1" \
	"$(said "$out" 'job `build` installs third-party tooling while holding a secret from the workflow-level `env:`: `pip install requests`')"
check "and the step did not silently pass it" "0" \
	"$(said "$out" 'no job holding a secret installs')"
check "and there is no parse-error warning hiding the miss" "0" \
	"$(said "$out" '::warning')"

# --- an unparseable workflow is a warning, not a silent skip -------------
d="$(new)"; printf 'name: [\n' > "$d/.github/workflows/broken.yml"
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "a workflow that does not parse does not fail the step" "0" "$rc"
check "but is reported as skipped, so the silence is visible" "1" "$(said "$out" '::warning')"

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
