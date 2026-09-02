#!/usr/bin/env bash
#
# .github/scripts/check-hardening.sh must pass a binary that carries every
# property and fail one that lacks exactly one, naming the property. The
# controls are built here with gcc, one flag away from the good one each, so a
# check that stopped looking at a property would show as its control passing.
#
# Requires: gcc with static-pie support (glibc 2.27 or later), readelf.
set -u

cd "$(dirname "$0")/../.." || exit 1
script=.github/scripts/check-hardening.sh
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
printf 'int main(void) { return 0; }\n' > "$tmp/m.c"
build() { # <name> <gcc flags...>
	local name=$1; shift
	if ! gcc "$@" -o "$tmp/$name" "$tmp/m.c" 2>/dev/null; then
		echo "FAIL  cannot build control '$name' with: $*"
		exit 1
	fi
}
build good       -static-pie -z now -z relro -s
build unstripped -static-pie -z now -z relro
build nopie      -static -no-pie -s
build dynamic    -pie -z now -z relro -s
build norelro    -static-pie -z now -z norelro -s
build lazy       -static-pie -z lazy -z relro -s
build execstack  -static-pie -z now -z relro -z execstack -s
printf 'not an elf\n' > "$tmp/text"

# The good one passes, on its own and with a sibling.
out=$(bash "$script" "$tmp/good"); rc=$?
check "a fully hardened static PIE passes" "0" "$rc"
check "and is reported as ok" "ok    $tmp/good" "$out"

# Each control fails, and the line names the property it lacks.
control() { # <name> <property>
	local out rc
	out=$(bash "$script" "$tmp/$1"); rc=$?
	check "$1 fails" "1" "$rc"
	check "$1 is failed for '$2'" "FAIL $2  $tmp/$1" "$out"
}
control unstripped stripped
control dynamic    static
control norelro    relro
control lazy       relro
control execstack  nx-stack

# A non-PIE static binary has no dynamic section at all, so it loses the PIE
# flag and BIND_NOW together; the property that matters is named first.
out=$(bash "$script" "$tmp/nopie"); rc=$?
check "nopie fails" "1" "$rc"
case "$out" in
	"FAIL pie"*) check "nopie is failed for 'pie' first" "yes" "yes" ;;
	*) check "nopie is failed for 'pie' first" "FAIL pie ..." "$out" ;;
esac

# One bad file among good ones fails the whole run, and the good ones still
# say ok, so the log shows which.
out=$(bash "$script" "$tmp/good" "$tmp/lazy" "$tmp/good"); rc=$?
check "one bad file among good ones fails the run" "1" "$rc"
check "the good ones are still listed as ok" "2" "$(printf '%s\n' "$out" | grep -c '^ok')"

# Not an ELF, and nothing at all.
out=$(bash "$script" "$tmp/text"); rc=$?
check "a non-ELF file fails" "1" "$rc"
check "and says it is not an ELF" "FAIL not-elf  $tmp/text" "$out"
bash "$script" >/dev/null 2>&1; rc=$?
check "no arguments is a usage error, exit 2" "2" "$rc"

echo
echo "passed: $pass  failed: $fail"
[ "$fail" -eq 0 ]
