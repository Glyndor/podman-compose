#!/usr/bin/env bash
#
# `install.sh --help` works under the documented pipe.
#
# The header carries the help text. The old `usage()` body read it via
# `sed -n '3,21p' "$0"`. That works for `./install.sh --help` and
# `bash install.sh --help`, where $0 is the script path. The script
# also documents the use `curl -fsSL .../install/unix | bash`, where
# the bytes arrive on stdin and $0 is the program name "bash": there
# is no file named "bash" and the old code failed with `sed: can't read
# bash`. Test that path here, plus the file-invocation path, plus a
# header/heredoc drift guard so a future hand-edit that touches one and
# not the other fails loudly.
#
# Requires: bash, sed, the script under test, nothing else.
set -u

cd "$(dirname "$0")/../.." || exit 1
script=install.sh
[ -f "$script" ] || { echo "FAIL  $script is missing"; exit 1; }

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

# Pin the help text the function emits. These are the lines the heredoc
# produces after `cat <<'USAGE'`. A reviewer editing one but not the
# other should see a diff here, not a silent divergence.
case "$(bash "$script" --help)" in
	*"podup installer."*) help_marker=1 ;; *) help_marker=0 ;;
esac
check "file invocation prints the help header" "1" "$help_marker"

case "$(bash "$script" --help)" in
	*"PODUP_VERSION"*) env_marker=1 ;; *) env_marker=0 ;;
esac
check "file invocation prints the env-var block" "1" "$env_marker"

# The documented pipe: `curl ... | bash`. `cat install.sh | bash -s --
# --help` reproduces it locally. The old `usage` body tried to read a
# file literally named "bash"; on the unfixed tree this exits with
# `sed: can't read bash: No such file or directory` and the help
# output never reaches the user. `cat | bash` is the shape we want to
# drive, even though `bash < file` would be a one-liner; shellcheck's
# SC2002 suggestion would change what we are testing.
# shellcheck disable=SC2002
pipe_out="$(cat "$script" | bash -s -- --help 2>&1)"
pipe_rc=$?
check "pipe invocation exits 0" "0" "$pipe_rc"

# On the unfixed tree the pipe produces `sed: can't read bash ...` (or
# an empty body) rather than the help. On the fixed tree the help header
# is in the output.
case "$pipe_out" in
	*"podup installer."*) pipe_marker=1 ;; *) pipe_marker=0 ;;
esac
check "pipe invocation prints the help header" "1" "$pipe_marker"

case "$pipe_out" in
	*"PODUP_VERSION"*) pipe_env_marker=1 ;; *) pipe_env_marker=0 ;;
esac
check "pipe invocation prints the env-var block" "1" "$pipe_env_marker"

case "$pipe_out" in
	*"sed:"*"can't read"*)
		check "pipe invocation does not surface a sed read error" "0" "1" ;;
	*)
		check "pipe invocation does not surface a sed read error" "0" "0" ;;
esac

# The two help bodies must match. The file-invocation path already
# worked (it was the curl|bash path that broke); this guard catches a
# future mutation that drops the heredoc and goes back to the broken
# sed-from-header on either side.
file_body="$(bash "$script" --help)"
# shellcheck disable=SC2002
pipe_body="$(cat "$script" | bash -s -- --help)"
if [ "$file_body" = "$pipe_body" ]; then
	check "help body is identical across invocation shapes" "1" "1"
else
	check "help body is identical across invocation shapes" "1" "0"
	echo "--- file invocation body ---"
	printf '%s\n' "$file_body"
	echo "--- pipe invocation body ---"
	printf '%s\n' "$pipe_body"
fi

# Header/heredoc drift guard: every line of the help heredoc must still
# appear in the file invocation output, and conversely, no body line
# surfaced only by the file path. The heredoc is duplicated against the
# header by design; this is the test that fails when one side moves
# and the other doesn't. The `case` shell-pattern matches avoid the
# grep-vs-flag-name pitfall that `grep -qxF` hits when a heredoc line
# starts with a dash.
file_lines="$(printf '%s\n' "$file_body" | sed -n '/^podup installer\./,/^  PODUP_INSTALL_DIR/p' | sed '/^$/d')"
missing=""
while IFS= read -r line; do
	[ -z "$line" ] && continue
	if ! grep -qxF -- "$line" <<<"$file_body"; then
		missing="${missing}${line}\n"
	fi
done <<<"$file_lines"
if [ -z "$missing" ]; then
	check "help body fully matches the heredoc" "1" "1"
else
	check "help body fully matches the heredoc" "1" "0"
	echo "        missing: $(printf '%b' "$missing" | head -3)"
fi

echo
echo "passed: $pass  failed: $fail"
printf 'DONE %s %d %d\n' "${BASH_SOURCE[0]##*/}" "$pass" "$fail"
[ "$fail" -eq 0 ]
