#!/usr/bin/env bash
#
# Every shell test must be invoked by a workflow, and every shell test a
# workflow invokes must exist. Both directions.
#
# This does NOT watch the Rust tests, and that is the point of writing it now
# rather than copying it from the distribution repositories earlier. cargo
# discovers its own tests and CI runs them with --all-features, so a Rust test
# cannot sit unregistered -- the analysis in the independence plan said exactly
# that, and it was right at the time, because this repository had no shell tests
# at all.
#
# Then tests/shell/locale-pinned.test.sh was added, and with it the one file
# type that only runs when a workflow names it. The conclusion did not survive
# the change that followed it. A second shell test added tomorrow would sit
# here, pass when anyone ran it by hand, and never execute in CI.
#
# In apt that happened for real: a guard against unpinned collation lived in
# tests/ for a day, passed locally, and no workflow called it.
#
# Comments do not count as wiring. The first version of this check in apt
# matched any mention of the path, so a test named only in prose read as
# invoked. Strip comment lines before looking.
#
# Requires: nothing beyond coreutils, grep and git.
set -u

cd "$(dirname "$0")/../.." || exit 1
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

# What the workflows actually run, with comment lines removed first.
# The leading `./` is OPTIONAL. It was required until #1595, and that made
# this function blind to how the fixtures are actually called: asset-contract
# invokes them as `bash tests/fixtures/releases/test.sh`, with no `./`,
# following the convention that names the interpreter rather than relying on
# an executable bit. Requiring the prefix meant the watcher saw an invocation
# only when it took the form it happened to have been written against.
invoked() {
	grep -rh -v '^[[:space:]]*#' .github/workflows/ 2>/dev/null \
		| grep -oE '(\./)?tests/[A-Za-z0-9_./-]+(\.test|test[A-Za-z0-9_-]*)\.sh' \
		| sed 's|^\./||' | LC_ALL=C sort -u
}

# What exists, from git rather than the working tree, so an untracked scratch
# file does not fail the run for someone mid-edit.
# Three naming conventions live here and all of them are shell tests.
# `tests/shell/` uses `*.test.sh`. `tests/fixtures/releases/` uses `test.sh`,
# `test-sign.sh` and `version-self-test.sh`, because they are fixtures a
# workflow drives rather than suites cargo could ever find.
#
# The glob matches `*test*` rather than enumerating those three. An
# enumeration is a copy of the tree, and the first draft of this one already
# proved it: written as `test.sh` and `test-*.sh` it missed
# `version-self-test.sh`, which a workflow does invoke, and the check went red
# against a repository that was correct.
#
# Only the first was watched until #1595, so the two files covering the
# release signing path could have been dropped from asset-contract.yml and
# nothing would have said so. That is the exact defect this file exists to
# catch, and it was sitting inside the file's own blind spot.
present() {
	git ls-files \
		'tests/**/*.test.sh' 'tests/*.test.sh' \
		'tests/fixtures/**/*test*.sh' \
		| LC_ALL=C sort -u
}

inv="$(invoked)"
pres="$(present)"

check "there is at least one shell test to account for" "1" \
	"$([ -n "$pres" ] && echo 1 || echo 0)"

check "every shell test is invoked by a workflow" "" \
	"$(comm -13 <(printf '%s\n' "$inv") <(printf '%s\n' "$pres"))"

check "every test a workflow invokes exists" "" \
	"$(comm -23 <(printf '%s\n' "$inv") <(printf '%s\n' "$pres"))"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
