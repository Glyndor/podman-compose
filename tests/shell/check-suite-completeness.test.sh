#!/usr/bin/env bash
#
# Run every test file the workflow invokes and require each one to print its
# sentinel. The sentinel is the last line every test file writes:
#
#     printf 'DONE %s %d %d\n' "${BASH_SOURCE[0]##*/}" "$pass" "$fail"
#
# It sits between the count line and `[ "$fail" -eq 0 ]`, so it is reached
# only after the last `check` runs. A test that exits early -- because
# someone planted `exit 0`, because `set -e` killed it on an unexpected
# error, or because anything else cut the run short -- never produces the
# sentinel, and this file names it.
#
# Why this shape, not a trap on EXIT: a trap fires on the truncated path
# too, so it would print the sentinel exactly when the file stopped early,
# which is the defect, not the fix. The sentinel is a plain `printf` that
# runs only when control reaches it.
#
# Why this shape, not a sidecar file: every test would have to know the
# sidecar path and append to it, and a non-writable sidecar is a new
# failure mode unrelated to what this catches.
#
# Why this shape, not parsing lint-shell.yml inside this file: the list
# of files is right there in the workflow, but a runner that parses YAML
# to find its own input couples itself to the workflow's shape. The
# workflow lists the tests as this file's arguments, and
# `tests/shell/ci-runs-every-test.test.sh` already keeps that list
# honest (every file invoked must exist, every file must be invoked).
#
# The macOS fixture skips cleanly where clang/otool/nm are absent; the
# skip path prints the sentinel before exiting 0, so the Linux job still
# sees it.
#
# Usage: check-suite-completeness.test.sh <test-file> [<test-file> ...]

set -u

cd "$(dirname "${BASH_SOURCE[0]}")/../.." || exit 1

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

if [ "$#" -eq 0 ]; then
	echo "usage: $0 <test-file> [<test-file> ...]" >&2
	exit 2
fi

self="${BASH_SOURCE[0]##*/}"

# Every test file must end with a sentinel of the form
# `DONE <basename> <pass-count> <fail-count>`. The basename is what the
# file itself sees in BASH_SOURCE, so the check is by basename rather than
# the path the runner was given: a file invoked as `./tests/foo.test.sh`
# and as `tests/foo.test.sh` both report `foo.test.sh` in their sentinel,
# and either way the runner reaches the same answer.
sentinel_re='^DONE [A-Za-z0-9_.-]+\.test\.sh [0-9]+ [0-9]+$'

for f in "$@"; do
	basename="${f##*/}"

	# Skip self if invoked recursively (it would not terminate).
	[ "$basename" = "$self" ] && continue

	output="$("$f" 2>&1)"
	rc=$?

	# The test's own output is the diagnosis: the FAIL line it printed
	# when one of its checks failed, or the `ok` line that was its last
	# printed word before a planted `exit 0`. Print it so the operator
	# who comes after the runner has the same picture.
	printf '%s\n' "$output"

	# The sentinel, if the test reached it, is the last line of output.
	# Command substitution strips trailing newlines, so the sentinel's
	# terminating \n is not in $output; the last line is the sentinel's
	# body.
	last_line="$(printf '%s' "$output" | tail -n 1)"
	has_sentinel=0
	if [ -n "$last_line" ] && [[ "$last_line" =~ $sentinel_re ]]; then
		has_sentinel=1
	fi

	check "$basename reached its end" "1" "$has_sentinel"
	if [ "$has_sentinel" -eq 0 ]; then
		echo "        last line was: $last_line"
	fi

	if [ "$rc" -ne 0 ]; then
		check "$basename exited 0" "1" "0"
	else
		check "$basename exited 0" "1" "1"
	fi
done

echo
echo "$pass passed, $fail failed"
printf 'DONE %s %d %d\n' "$self" "$pass" "$fail"
[ "$fail" -eq 0 ]
