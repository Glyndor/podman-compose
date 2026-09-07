#!/usr/bin/env bash
#
# Decide whether the newest completed run of a workflow on a branch is a
# pass. Extracted from the step in `reusable-branch-health.yml` so the
# conclusion logic can be exercised against planted API answers without
# `gh` or the network.
#
# Reads the GitHub API response on stdin (the JSON returned by
# `repos/:owner/:repo/actions/workflows/:workflow/runs?branch=:branch&status=completed&per_page=1`),
# prints a one-line summary on stdout, and exits 0 only when the
# newest run's `conclusion` is `success`.
#
# Exits 1 otherwise. The line printed is the same one the workflow step
# surfaces, so a planted test can match on it.

set -euo pipefail

workflow=${WORKFLOW:?WORKFLOW env var is required}
branch=${BRANCH:?BRANCH env var is required}

# Pull both fields out of the response in a single `jq` call. `jq` reads
# stdin exactly once, so two separate calls would race for the input and
# the second would see nothing. `gh api ... --jq` gives exactly the same
# shape on a runner; here `jq` is the same dependency the workflow
# already requires for the JSON parsing elsewhere, so this script does
# not introduce a new tool.
#
# Each filter emits one line, in order, so two reads pick them up cleanly.
# Joining them on a single tab-separated line and slicing with shell
# parameter expansion would also work; two reads is the shape the rest of
# the test suite uses (`grep | sed | while read`) and reads the same way.
fields="$(jq -r '.workflow_runs[0].conclusion // empty, .workflow_runs[0].html_url // empty')"
conclusion="$(printf '%s\n' "$fields" | sed -n '1p')"
run_url="$(printf '%s\n' "$fields" | sed -n '2p')"

if [ -z "$conclusion" ]; then
	echo "::error::No completed run of ${workflow} on record for branch ${branch}."
	echo "A protected branch with no completed run is not a branch to release from." >&2
	exit 1
fi

echo "Newest completed run of ${workflow} on ${branch}: conclusion=${conclusion}, url=${run_url}"

case "$conclusion" in
	success)
		echo "Within the success threshold."
		exit 0
		;;
	failure)
		echo "::error::Newest completed run of ${workflow} on ${branch} failed: ${run_url}"
		exit 1
		;;
	cancelled)
		# A cancelled run is a suite that was interrupted, not one that
		# passed. Reporting it as success would let a published branch
		# carry a "we never finished" verdict; reporting it as a generic
		# failure loses the reason. So name it.
		echo "::error::Newest completed run of ${workflow} on ${branch} was cancelled: ${run_url}"
		echo "A cancelled run on a protected branch is a published state whose suite was interrupted." >&2
		exit 1
		;;
	skipped)
		# Skipped is the workflow's own gate deciding not to run for the
		# branch. On a branch whose `on:` is supposed to cover it, that is
		# the gate not exercising the protection the comment claims.
		echo "::error::Newest completed run of ${workflow} on ${branch} was skipped: ${run_url}"
		echo "A skipped run on a protected branch is the workflow deciding not to exercise the gate." >&2
		exit 1
		;;
	*)
		# Any other conclusion (timed_out, action_required, neutral,
		# startup_failure, stale) is also not success. The workflow does
		# not enumerate them, and this script is the single place that
		# decides which ones to enumerate.
		echo "::error::Newest completed run of ${workflow} on ${branch} has unexpected conclusion ${conclusion}: ${run_url}"
		exit 1
		;;
esac
