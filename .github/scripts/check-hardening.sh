#!/usr/bin/env bash
#
# Assert the hardening the Linux release binaries are documented with, over
# every ELF named on the command line. Five properties, each read with readelf:
#
#   static PIE     ET_DYN with the PIE flag and no PT_INTERP: the binary
#                  relocates on every exec and brings its own libc
#   full RELRO     a GNU_RELRO segment and BIND_NOW, so the GOT is read-only
#                  before main runs
#   NX stack       GNU_STACK without the E flag
#   stripped       no .symtab; the release profile says strip = true
#
# These come from rustc's defaults for the musl target and from
# [profile.release]; nothing in the repository asks for them by name, so a
# toolchain bump or a RUSTFLAGS in a build step could drop any one and the
# release would still sign and publish. This is where that is caught.
#
# One line per file, "ok" or "FAIL <property>", and a non-zero exit when any
# file fails any property. readelf output is read under LC_ALL=C because the
# headings are translated ("Tipo:" for "Type:").
#
# Requires: readelf (binutils), grep, awk.
set -u
export LC_ALL=C

if [ "$#" -eq 0 ]; then
	echo "usage: $0 <elf>..." >&2
	exit 2
fi

status=0
for f in "$@"; do
	if ! header=$(readelf -hW "$f" 2>/dev/null); then
		echo "FAIL not-elf  $f"
		status=1
		continue
	fi
	segments=$(readelf -lW "$f")
	dynamic=$(readelf -dW "$f" 2>/dev/null || true)
	sections=$(readelf -SW "$f")
	failed=""

	type=$(printf '%s\n' "$header" | awk '/^ *Type:/ {print $2}')
	if [ "$type" != "DYN" ] || ! printf '%s\n' "$dynamic" | grep -q 'FLAGS_1.*PIE'; then
		failed="$failed pie"
	fi
	if printf '%s\n' "$segments" | grep -q 'INTERP'; then
		failed="$failed static"
	fi
	if ! printf '%s\n' "$segments" | grep -q 'GNU_RELRO' \
		|| ! printf '%s\n' "$dynamic" | grep -q 'BIND_NOW'; then
		failed="$failed relro"
	fi
	stack=$(printf '%s\n' "$segments" | awk '/GNU_STACK/ {print $7}')
	case "$stack" in
		RW) ;;
		*) failed="$failed nx-stack" ;;
	esac
	if printf '%s\n' "$sections" | grep -q '\.symtab'; then
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
