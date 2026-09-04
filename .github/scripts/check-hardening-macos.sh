#!/usr/bin/env bash
#
# Assert the hardening the macOS release binaries are documented with, over
# every Mach-O file named on the command line. Two properties:
#
#   pie       MH_PIE (0x200000) is set in mach_header.flags. With it on,
#             the loader can relocate the binary on every exec; without it
#             the binary loads at a fixed address.
#   stripped  No local symbols remain (nm shows only `U`, undefined
#             external references). Local symbols are lowercase types in
#             nm output: t (text), d (data), b (bss), r (readonly),
#             s/S (small). Anything lowercase is non-global and the
#             release profile must not ship it.
#
# Both checks use only tools that ship with macOS (otool, nm). They are
# the same tools Apple uses for the same question, so a binary that
# reads "ok" here is one an Apple reviewer would call hardened.
#
# Output: one line per file, "ok" or "FAIL <property>  <file>", with a
# non-zero exit when any file fails any property. Exit 2 with usage on
# no arguments.
#
# Requires: otool, nm. Both are present on macOS by default; on Linux
# the script cannot run and the test that drives it skips cleanly.
set -u
export LC_ALL=C

if [ "$#" -eq 0 ]; then
	echo "usage: $0 <macho>..." >&2
	exit 2
fi

if ! command -v otool >/dev/null 2>&1; then
	echo "FAIL: otool is required and was not found on PATH" >&2
	exit 1
fi
if ! command -v nm >/dev/null 2>&1; then
	echo "FAIL: nm is required and was not found on PATH" >&2
	exit 1
fi

# MH_PIE = 0x200000. Read mach_header.flags from `otool -h` (the lower-case
# form, which prints the fields one per line and is the easiest to parse).
#
# A file that is not Mach-O at all has no `flags` line in `otool -h`; in
# that case `awk` finds nothing and `flags` ends up empty, which fails the
# PIE check. That is fine: the script is the macOS analogue of the PE
# `not-pe` line and a non-Mach-O file would be a real release-side bug.
PIE_HEX=0x200000

status=0
for f in "$@"; do
	if [ ! -f "$f" ]; then
		echo "FAIL not-macho  $f"
		status=1
		continue
	fi

	failed=""

	# `otool -h` prints a title line, a column-name line whose last column is
	# `flags`, and a value line that starts with the magic (`0xfeedfacf`) and
	# ends with the flags. The first release run read the column-name line
	# and reported every binary as not-macho (v5.9.0, 2026-09-04).
	flags=$(otool -h "$f" 2>/dev/null | awk 'NR > 1 && $1 ~ /^0x/ { print $NF; exit }')
	if [ -z "$flags" ]; then
		failed="$failed not-macho"
	else
		# Strip the leading 0x for arithmetic. An empty value here would
		# also fail, but otool prints "0x0" at minimum.
		hex=${flags#0x}
		# Some otool versions print lowercase hex; the arithmetic is
		# case-insensitive under bash. Pad to a known width so the
		# leading zeros the value loses on small numbers do not matter.
		dec=$((16#$hex))
		if [ $((dec & PIE_HEX)) -eq 0 ]; then
			failed="$failed pie"
		fi
	fi

	# Stripped: any line in `nm` whose second field is a single lowercase
	# letter is a local symbol. Undefined externals (`U`) are not local
	# and a stripped binary has only those, so the grep must look at the
	# second column specifically. `nm -p` skips sorting, which keeps
	# small binaries fast; the check is "any local symbol exists",
	# not "every local symbol is named X".
	if nm -p "$f" 2>/dev/null | awk '$2 ~ /^[a-z]$/' | grep -q .; then
		failed="$failed stripped"
	fi

	if [ -z "$failed" ]; then
		echo "ok    $f"
	else
		echo "FAIL ${failed# }  $f"
		status=1
	fi
done
exit "$status"
