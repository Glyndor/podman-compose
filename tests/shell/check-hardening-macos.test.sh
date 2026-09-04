#!/usr/bin/env bash
#
# .github/scripts/check-hardening-macos.sh must pass a Mach-O binary that
# is PIE and stripped and fail one that lacks exactly one of those
# properties, naming the property. The controls are built here with
# clang, one flag away from the good one each, so a check that stopped
# looking at one property would show as its control passing.
#
# Skip where clang or otool are absent. Both are present on the macOS
# GitHub Actions runner; the test only runs there (CI invokes it from
# the release workflow's macOS leg).
#
# Requires: clang, otool, nm.
set -u

cd "$(dirname "$0")/../.." || exit 1
script=.github/scripts/check-hardening-macos.sh

if ! command -v clang >/dev/null 2>&1 || ! command -v otool >/dev/null 2>&1 || ! command -v nm >/dev/null 2>&1; then
	echo "SKIP: clang/otool/nm are not all on PATH; this test runs on the macOS CI runner only."
	exit 0
fi

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

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# A Mach-O that the linker accepts as a standalone executable. The body
# is irrelevant: the script does not execute the binary, only reads
# its header.
# A static function gives the binary one local symbol (`t _helper`), which is
# what `-Wl,-x` strips and what the `stripped` check looks for. A `main` alone
# has no local symbol, so the unstripped control passed the check on the
# first run of this fixture on a Mac (2026-09-04).
printf 'static int helper(void) { return 1; }\nint main(void) { return helper(); }\n' > "$tmp/m.c"

build() { # <name> <clang flags...>
	local name=$1; shift
	if ! clang "$@" -o "$tmp/$name" "$tmp/m.c" 2>/dev/null; then
		echo "FAIL  cannot build control '$name' with: $*"
		exit 1
	fi
}
# A good Mach-O: PIE and stripped. `-Wl,-x` is what strips the local
# symbols at link time; the release profile's `strip = true` does the
# same on the cargo side. `-Wl,-pie` forces the linker to emit MH_PIE.
build good       -Wl,-pie -Wl,-x
# No PIE: `-Wl,-no_pie` is the linker flag; this is the control for the
# pie property. It is still stripped.
# ld64 ignores -no_pie for arm64 (every arm64 Mach-O is PIE), so on an Apple
# Silicon runner this control came out PIE and passed the check on the
# fixture's first run on a Mac (2026-09-04). The control is built for x86_64,
# where the flag is honoured; otool reads the flags of either architecture.
build nopie      -arch x86_64 -Wl,-no_pie -Wl,-x
# PIE but unstripped: the linker keeps local symbols (lowercase types
# in nm). This is the control for the stripped property.
build unstripped -Wl,-pie
printf 'not a macho\n' > "$tmp/text"

# The good one passes, on its own and with a sibling.
out=$(bash "$script" "$tmp/good"); rc=$?
check "a fully hardened PIE+stripped Mach-O passes" "0" "$rc"
check "and is reported as ok" "ok    $tmp/good" "$out"

# Each control fails, and the line names the property it lacks.
control() { # <name> <property>
	local out rc
	out=$(bash "$script" "$tmp/$1"); rc=$?
	check "$1 fails" "1" "$rc"
	check "$1 is failed for '$2'" "FAIL $2  $tmp/$1" "$out"
}
control nopie      pie
control unstripped stripped

# A non-Mach-O file fails the same way as the Linux `not-elf` line,
# named `not-macho` for the format.
out=$(bash "$script" "$tmp/text"); rc=$?
check "a non-Mach-O file fails" "1" "$rc"
check "and says it is not a Mach-O" "FAIL not-macho  $tmp/text" "$out"

# One bad file among good ones fails the run, and the good ones still
# say ok, so the log shows which.
out=$(bash "$script" "$tmp/good" "$tmp/nopie" "$tmp/good"); rc=$?
check "one bad file among good ones fails the run" "1" "$rc"
check "the good ones are still listed as ok" "2" "$(printf '%s\n' "$out" | grep -c '^ok')"

# No arguments is a usage error, exit 2.
bash "$script" >/dev/null 2>&1; rc=$?
check "no arguments is a usage error, exit 2" "2" "$rc"

echo
echo "passed: $pass  failed: $fail"
[ "$fail" -eq 0 ]
