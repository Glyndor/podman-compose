#!/usr/bin/env bash
#
# `check-branch-conclusion.sh` decides whether the newest completed run of a
# workflow on a branch is a pass. It is the only piece of behaviour in the
# branch-health guard, and the rest of it is a YAML wiring the shell script
# is fed by `gh api`. A test that exercises the wiring but not the decision
# proves nothing about the decision.
#
# The harness here plants API answers on stdin and asserts exit code plus
# the matching summary line. Three states the brief calls out by name --
# `success` passes, `failure` fails, an empty list reports "no run on
# record" -- are covered with the same JSON the GitHub API returns today,
# plus `cancelled` and `skipped`, which the reusable's comment says it
# treats as failures (the comment above the script's `case` is the
# contract this test pins).
#
# Each plant is a complete `repos/.../runs` response, not just the array,
# because the script reads `.workflow_runs[0]` and would behave differently
# against a bare array vs. an envelope. Building plants out of the envelope
# is the cheapest way to read what the script reads.
#
# Requires: bash, jq, the script under test, nothing else.
set -u

cd "$(dirname "$0")/../.." || exit 1
script=.github/scripts/check-branch-conclusion.sh
[ -x "$script" ] || { echo "FAIL  $script is not executable in git"; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "FAIL  jq is required to run this test"; exit 1; }

pass=0; fail=0

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

# Run the script under test against a planted API answer. The script
# reads WORKFLOW and BRANCH from the environment, so the harness passes
# them in.
run_with() { # <workflow> <branch> <json-on-stdin>
	WORKFLOW="$1" BRANCH="$2" bash "$script" <<<"$3"
}

# Each plant is a complete response from the workflow-runs endpoint, with
# exactly one entry under `.workflow_runs`. The shape comes from the
# GitHub docs and the value the script reads is `.workflow_runs[0].conclusion`.

plant_success='{"workflow_runs":[{"id":1,"conclusion":"success","html_url":"https://x/r/1"}]}'
plant_failure='{"workflow_runs":[{"id":2,"conclusion":"failure","html_url":"https://x/r/2"}]}'
plant_cancelled='{"workflow_runs":[{"id":3,"conclusion":"cancelled","html_url":"https://x/r/3"}]}'
plant_skipped='{"workflow_runs":[{"id":4,"conclusion":"skipped","html_url":"https://x/r/4"}]}'
plant_empty='{"workflow_runs":[],"total_count":0}'

# --- success passes ---
out=$(run_with "ci.yml" "main" "$plant_success"); rc=$?
check "success on main exits 0" "0" "$rc"
check "success on main prints the url" "https://x/r/1" \
	"$(printf '%s\n' "$out" | grep -oE 'https://x/r/[0-9]+' || true)"

# --- failure fails ---
out=$(run_with "ci.yml" "main" "$plant_failure"); rc=$?
check "failure on main exits 1" "1" "$rc"
check "failure on main names the conclusion" "failure" \
	"$(printf '%s\n' "$out" | grep -oE 'conclusion=f[a-z]+' | head -1 | cut -d= -f2)"

# --- cancelled fails and explains why ---
out=$(run_with "ci.yml" "main" "$plant_cancelled"); rc=$?
check "cancelled on main exits 1" "1" "$rc"
case "$out" in
	*cancelled*) check "cancelled on main is named" "yes" "yes" ;;
	*) check "cancelled on main is named" "cancelled-named" "$(printf '%s\n' "$out" | head -1)" ;;
esac

# --- skipped fails and explains why ---
out=$(run_with "ci.yml" "main" "$plant_skipped"); rc=$?
check "skipped on main exits 1" "1" "$rc"
case "$out" in
	*skipped*) check "skipped on main is named" "yes" "yes" ;;
	*) check "skipped on main is named" "skipped-named" "$(printf '%s\n' "$out" | head -1)" ;;
esac

# --- empty history fails with "no completed run on record" ---
out=$(run_with "ci.yml" "main" "$plant_empty"); rc=$?
check "empty history on main exits 1" "1" "$rc"
case "$out" in
	*"No completed run"*) check "empty history says no run on record" "yes" "yes" ;;
	*) check "empty history says no run on record" "no-run-on-record" "$(printf '%s\n' "$out" | head -1)" ;;
esac

# --- the script's branch parameter actually surfaces in the message ---
# Same input, different branch argument: the failure message names the
# branch the caller asked about, not whichever the harness happens to use.
out_main=$(run_with "ci.yml" "main" "$plant_failure")
out_develop=$(run_with "ci.yml" "develop" "$plant_failure")
case "$out_main" in
	*"on main"*) check "failure message names branch=main" "yes" "yes" ;;
	*) check "failure message names branch=main" "branch=main" "$(printf '%s\n' "$out_main" | head -1)" ;;
esac
case "$out_develop" in
	*"on develop"*) check "failure message names branch=develop" "yes" "yes" ;;
	*) check "failure message names branch=develop" "branch=develop" "$(printf '%s\n' "$out_develop" | head -1)" ;;
esac

# --- the script refuses to run without WORKFLOW or BRANCH ---
out=$(WORKFLOW='' BRANCH=main bash "$script" <<<"$plant_success" 2>/dev/null); rc=$?
check "missing WORKFLOW is fatal" "1" "$rc"
out=$(WORKFLOW=ci.yml BRANCH='' bash "$script" <<<"$plant_success" 2>/dev/null); rc=$?
check "missing BRANCH is fatal" "1" "$rc"

# --- the negative control: a planted response the parser is asked to
# reject, with a conclusion that does not exist in the API. The script
# falls through to the default branch, which is "not success" rather
# than silently passing. ---
plant_unknown='{"workflow_runs":[{"id":5,"conclusion":"made_up","html_url":"https://x/r/5"}]}'
out=$(run_with "ci.yml" "main" "$plant_unknown"); rc=$?
check "unknown conclusion exits 1" "1" "$rc"
case "$out" in
	*"unexpected conclusion made_up"*) check "unknown conclusion is named" "yes" "yes" ;;
	*) check "unknown conclusion is named" "unexpected-made_up" "$(printf '%s\n' "$out" | head -1)" ;;
esac

echo
echo "passed: $pass  failed: $fail"
[ "$fail" -eq 0 ]
