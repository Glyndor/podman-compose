#!/usr/bin/env bash
#
# Every `sort` in a shell script must run under a fixed collation.
#
# UTF-8 collations ignore punctuation at the primary level, so `sort -u`
# considers `pod-up` and `podup` EQUAL and drops one of them. Both are legal
# names -- for a Debian package, for a container image, for anything a script
# collects and deduplicates. The runner's locale is not the developer's, so a
# script that passes on one machine can silently drop an entry on another, and
# the output looks like a shorter list rather than an error.
#
# Measured in Glyndor/apt on 2026-08-24: under en_US.UTF-8 a two-entry list
# came back with one entry and nothing said so.
#
# This is the one meta-test from the distribution trio that transfers here.
# `ci-runs-every-test` does not: cargo discovers the Rust tests and CI runs them
# with --all-features, so a test cannot sit unregistered the way a shell file
# can. Its real analogue -- code compiled only under debian/rules' narrower
# feature set -- is already covered by debian-build.yml's path filters, which
# carry the #1295 lesson in their own comments.
#
# Requires: nothing beyond coreutils and grep.
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

# Every tracked shell script, so a new one is covered the day it lands rather
# than when someone remembers to add it here. Comments do not count: a `sort`
# inside one is not executed, and matching them would make the check unfixable
# by explaining the rule next to the code that follows it.
scripts="$(git ls-files '*.sh' | grep -v '^tests/shell/')"
check "there are shell scripts to check" "1" \
	"$([ -n "$scripts" ] && echo 1 || echo 0)"

unpinned=""
for f in $scripts; do
	hits="$(grep -nE '(^|[|;&(]|\$\()[[:space:]]*sort\b' "$f" 2>/dev/null \
		| grep -v '^[0-9]*:[[:space:]]*#' \
		| grep -v 'LC_ALL' || true)"
	[ -n "$hits" ] && unpinned="$unpinned$f: $hits"$'\n'
done

check "every sort runs under a pinned collation" "" "${unpinned%$'\n'}"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
