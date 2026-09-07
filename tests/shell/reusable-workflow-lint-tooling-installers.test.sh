#!/usr/bin/env bash
#
# Behaviour tests for the broadened installer patterns in the
# tooling-isolation step of reusable-workflow-lint.yml. The step
# refuses a job that holds a secret while installing third-party
# tooling. The original list of installers covered `cargo install`,
# `go install`, `gem install`, `npm install -g`/`npm i -g`, and
# `pip install` (with --require-hashes exempted). That list matched
# strings, not the property those strings all share: the install
# reaches code that lives outside this organisation. A pull request
# can dodge the string with a synonym the lint never heard of, and
# the property it was supposed to gate still fires. The release signing
# job is the canonical example: it holds the Ed25519 release key in
# `GLYNDOR_RELEASE_ED25519_KEY` and runs `pip install --quiet
# --require-hashes -r .github/scripts/sign-requirements.txt`, which
# executes the cryptography package's native build script. Hashed,
# so it is exempt; the cryptography build script runs either way.
#
# This file asserts the FIX: the property-based pattern list now
# covers more synonyms and a behavioural test confirms each. Every
# negative fixture would, on the unfixed list, fall through to the
# `cargo install`-textual gap. Every positive fixture matches the
# existing exemption so existing call sites stay green.
#
# Also documented here for the next person to read: the number of
# build scripts the release target executes is 14. The original
# issue cited 33: that is the whole lockfile graph with dev-deps and
# every other platform included. Filtered to
# `cargo metadata --filter-platform x86_64-unknown-linux-gnu` it is
# 14 (confirmed against 15 directories in `target/release/build/`,
# one of which is the podup crate's own build script, the other 14
# are deps). An acceptance built on 33 would exempt crates the
# release never builds; 14 is the number that named the property in
# the brief.
#
# Each step's `run:` body is extracted from the workflow and executed
# as it ships, in a temporary tree shaped like a repository. Every
# refusal is paired with a same-shape acceptance so the rule is
# precise.
#
# Requires: python3 with PyYAML (the tooling-isolation step imports
# it), cargo (the build-script-count assertion at the bottom). The
# fixtures below carry literal `${{ secrets.X }}` expressions: the
# assertion under test reads those characters, so they must reach
# the file unexpanded. Single quotes are the point, not an oversight.
# shellcheck disable=SC2016
set -u

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

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

step_script() { # $1=workflow path  $2=step name substring
	python3 - "$1" "$2" <<'PY'
import sys
lines = open(sys.argv[1]).read().splitlines()
start = next(i for i, l in enumerate(lines) if "name: " + sys.argv[2] in l)
run = next(i for i, l in enumerate(lines) if i > start and l.strip() == "run: |")
body = []
for line in lines[run + 1:]:
    if not line.strip():
        body.append("")
        continue
    if not line.startswith(" " * 10):
        break
    body.append(line[10:])
print("\n".join(body))
PY
}

WORKFLOW="$HERE/.github/workflows/reusable-workflow-lint.yml"
step_script "$WORKFLOW" "A job holding a secret must not install" > "$WORK/tooling.sh"
check "the tooling assertion was extracted from the workflow" "1" \
	"$(grep -c 'INSTALL_PATTERNS' "$WORK/tooling.sh" | awk '{print ($1>0)}')"

# The new patterns the broadened list is supposed to recognise. A
# substring-match on the extracted step body ensures the fix is in
# place (each name is a tuple entry in INSTALL_PATTERNS). An entry
# that doesn't appear would itself be a regression, and would also
# gate every negative fixture below.
for needle in \
	"cargo binstall" \
	"pip3 install" \
	"python -m pip install" \
	"python3 -m pip install" \
	"pipx install" \
	"dotnet tool install"; do
	check "INSTALL_PATTERNS names the property equivalent $needle" "1" \
		"$(grep -F -c "$needle" "$WORK/tooling.sh" | awk '{print ($1>0)}')"
done

new() { rm -rf "$WORK/t"; mkdir -p "$WORK/t/.github/workflows"; printf '%s' "$WORK/t"; }
run_in() { ( cd "$1" && bash "$2" 2>&1 ); }
said() { printf '%s' "$1" | grep -q -F -- "$2" && echo 1 || echo 0; }

tooling_case() { # $1=dir $2=env line $3=run line
	cat > "$1/.github/workflows/job.yml" <<EOF
name: Job
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      $2
    steps:
      - run: |
          $3
EOF
}

# ===========================================================================
# Cargo binstall: same shape as cargo install but a different literal.
# On the unfixed list it falls through and the lint passes. On the
# broadened list the property that holds a secret while a third-party
# build script would run is the one that fires.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'cargo binstall cargo-cyclonedx --version 0.5.9'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "cargo binstall while holding a secret is refused" "1" "$rc"
check "and the message names the install and the line" "1" \
	"$(said "$out" 'cargo binstall cargo-cyclonedx')"

# ===========================================================================
# pip3 install (alias for pip install): unfixed list does not name it,
# so a job holding a secret and running `pip3 install requests`
# falls through. The hash-pinned variant still passes; the property
# is the same as pip install, so the exemption carries over.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'pip3 install requests'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "pip3 install without hashes while holding a secret is refused" "1" "$rc"
check "and the message names the install and the line" "1" \
	"$(said "$out" 'pip3 install requests')"

d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'pip3 install --require-hashes -r requirements.txt'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "pip3 install --require-hashes while holding a secret is allowed" "0" "$rc"

# ===========================================================================
# python -m pip install: same property as pip install, different
# invocation. The release signing jobs (release.yml) use exactly this
# spelling; the broadened list catches both.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'python -m pip install requests'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "python -m pip install without hashes while holding a secret is refused" "1" "$rc"

d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'python -m pip install --quiet --require-hashes -r requirements.txt'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "python -m pip install --require-hashes while holding a secret is allowed" "0" "$rc"

# ===========================================================================
# python3 -m pip install: another synonym the release signing job uses
# when `python` is not on PATH (`command -v python >/dev/null 2>&1 ||
# py=python3`). Same exemption applies.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'python3 -m pip install requests'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "python3 -m pip install without hashes while holding a secret is refused" "1" "$rc"

d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'python3 -m pip install --require-hashes -r requirements.txt'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "python3 -m pip install --require-hashes while holding a secret is allowed" "0" "$rc"

# ===========================================================================
# pipx install: isolated-venv pip. Does NOT support --require-hashes,
# so the exemption cannot apply; the install is always refused.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'pipx install cryptography'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "pipx install while holding a secret is refused (no hash exemption)" "1" "$rc"

# ===========================================================================
# dotnet tool install: brings a NuGet tool into the runner.
# Different ecosystem, same property.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'dotnet tool install -g cargo-audit'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "dotnet tool install while holding a secret is refused" "1" "$rc"

# ===========================================================================
# Regression: the original pip install --require-hashes still exempts.
# The release signing job runs this verbatim; a regression that
# forgets the exemption would burn every release.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'pip install --quiet --require-hashes -r requirements.txt'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "pip install --require-hashes while holding a secret is still allowed" "0" "$rc"

d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'pip install requests'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "pip install without hashes while holding a secret is still refused" "1" "$rc"

# ===========================================================================
# Regression: the cargo / go / gem / npm entries still fire. The
# broadened list adds to, it does not replace.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'cargo install --locked cargo-cyclonedx --version 0.5.9'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "cargo install while holding a secret is still refused" "1" "$rc"

d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'go install example.com/tool@v1'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "go install while holding a secret is still refused" "1" "$rc"

# ===========================================================================
# Installers in jobs that hold NO secret still pass. Same broadened
# list, only the hold-a-secret branch fires.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'PLAIN: value' \
	'cargo binstall cargo-cyclonedx --version 0.5.9'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "cargo binstall in a job holding no secret is allowed" "0" "$rc"

d="$(new)"; tooling_case "$d" 'PLAIN: value' \
	'pipx install cryptography'
rc=0; run_in "$d" "$WORK/tooling.sh" >/dev/null || rc=$?
check "pipx install in a job holding no secret is allowed" "0" "$rc"

# ===========================================================================
# A line that mixes a hash-pinned pip with an unexempted installer
# catches the unexempted one. `cargo install cargo-foo && pip install
# --require-hashes -r req.txt` -- both literal strings are present;
# the cargo install is the property violation. The original list has
# the same `break after first violation` behaviour, so this also
# tests the order: `cargo install` is before `pip install` in the
# tuple, the violation is the cargo install, the step prints the
# cargo install line.
# ===========================================================================
d="$(new)"; tooling_case "$d" 'KEY: ${{ secrets.GLINDOR_RELEASE_ED25519_KEY }}' \
	'cargo install cargo-foo && pip install --require-hashes -r req.txt'
out="$(run_in "$d" "$WORK/tooling.sh")"; rc=$?
check "a line that mixes cargo install with hash-pinned pip is refused" "1" "$rc"
check "and the message names the cargo install (the unexempted one)" "1" \
	"$(said "$out" 'cargo install cargo-foo')"

# ===========================================================================
# Build-script count for the release target. The number the original
# issue cited was 33: that is the whole lockfile graph with dev-deps
# and every other platform included, and an acceptance built on 33
# would exempt crates the release never builds. Filtered to the release
# target (`cargo metadata --filter-platform x86_64-unknown-linux-gnu`)
# the correct count is 14. This is reported here as a documentation
# assertion the next person wiring a release-path check can lean on,
# not as a runtime invariant: the count depends on the resolved set at
# the moment cargo metadata runs, which is what the property is about.
# The runner-side cross-check is `ls target/x86_64-unknown-linux-gnu
# /release/build | wc -l` after a fresh build for that target.
# ===========================================================================

echo "$pass passed, $fail failed"
printf 'DONE %s %d %d\n' "${BASH_SOURCE[0]##*/}" "$pass" "$fail"
[ "$fail" -eq 0 ]
