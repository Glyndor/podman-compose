# Contributing to Glyndor/podup

This repository has its own guide because the organisation's shared one
describes a `topic → develop → main` flow that names branch flow and
checklists that do not match what the workflows actually do here.
Following it would tell you to push to a branch that is not where work
lands.

Contributions are invitation-only. Bug reports and ideas through issues
are welcome; unsolicited pull requests are not accepted.

## What this repository is

It builds and ships **podup**, a docker-compose translator and runner for
rootless Podman. A single static Rust binary, with no daemon and no
Python runtime. The `podup` binary is what end users install; there is a library
target and the integration tests build against it, but it is not published to
crates.io and carries no semver promise about its Rust API.

Nothing automated writes to this repository's git. `release.yml` builds
binaries and attaches them to a GitHub Release; it never commits back.

## Branch flow

```
topic branch ──squash──▶ develop ──merge commit──▶ main
```

- **Branch from `develop`.** Pull requests target `develop`, not `main`.
  The exception is the release pull request, which targets `main` and is
  the only path that crosses the `develop → main` boundary. A reusable
  (`reusable-main-guard.yml`) enforces that: any other pull request into
  `main` fails the `develop-only` check before it can merge.
- **Squash-merge into `develop.** The pull request title is the squashed
  commit message, written in Conventional Commit form.
- **Merge-commit into `main.** The release pull request is a real merge
  commit, so `develop` keeps the history of every fix until the release
  is cut. Tags come off `main` only; `release.yml` refuses a tag that is
  not reachable from `origin/main`.

`Closes #N` in a pull request that lands on `develop` does not auto-close
the issue, because the default branch is `main` and GitHub only closes
from there. Mention the issue in the pull request body and add the label
that marks the work done; closing happens when the release pull request
merges.

## Before you open a pull request

- **An issue first.** Labels are the tracking system here; there is no
  board. Apply `type:`, `prio:`, `effort:`, `status:` and `area:` where
  they fit.
- **Sign every commit off**, with `git commit -s`. The `dco` check is
  required and is the only thing standing behind that attestation.
- **Commits are signed**, GPG or SSH. `required_signatures` is enforced
  on `develop` and on `main`, and rebase-merge is disabled because GitHub
  re-creates rebased commits without signatures.
- **Conventional Commit title** on the pull request. It becomes the
  squashed commit message.

## Tests

Run the suite before pushing:

```sh
fail=0
cargo fmt --all --check || fail=1
cargo clippy --locked --all-targets --all-features -- -D warnings || fail=1
cargo test --locked --all-features --workspace || fail=1
for t in tests/shell/*.test.sh; do bash "$t" || { echo "FAILED: $t"; fail=1; }; done
# shellcheck covers every tracked *.sh and every extensionless script with
# a shell shebang; the workflow runs the same find + xargs pipeline.
find . -type f \( -name '*.sh' -o -name '*.bash' \) -not -path './.git/*' \
  -print0 | xargs -0 -r shellcheck --severity=style || fail=1
[ "$fail" -eq 0 ] && echo "all green" || echo "SOMETHING FAILED"
```

The `fail` flag is not decoration. Running everything instead of stopping at
the first red is what you want locally, but it means the block ends on the
last command and reports whatever that one did. The loop was worse than the
rest: `|| echo` swallows the status outright, so it printed `FAILED: <file>`
and still left `$?` at zero. Without the flag, the status you glance at
disagrees with the output you scrolled past.

The Rust toolchain is pinned by `reusable-rust-ci.yml`'s `toolchain` input,
and the MSRV by `ci.yml`'s `msrv` input. Read the values there rather than
from here: nine workflow sites write the toolchain and two write the MSRV,
`tests/workflow_toolchain_pin/` asserts that each group agrees on every pull
request, and `pin-watch.yml` compares the toolchain against upstream stable
every Monday. A number copied into this file would be a tenth site that no
check reads.

Two rules matter more than coverage:

**A shell test no workflow runs is indistinguishable from one that
passes.** `./tests/shell/ci-runs-every-test.test.sh` fails when a file in
`tests/shell/` is not named in any workflow's `test-command`, and when a
workflow names one that does not exist. Both directions. cargo discovers
its own Rust tests and CI runs them with `--all-features`, so the same
watcher is unnecessary for the Rust suite and was deliberately not
copied from the distribution repositories.

**A test you have not watched fail is not a test.** Before claiming a
check works, delete or invert the control it covers, run it, and confirm
it goes red for the reason it names. Three ways that goes wrong are
written up in `standards/testing`, "Three ways a sabotage lies to you":
a sabotage that changes nothing, one that changes something the test
does not look at, and one where the red comes from somewhere else
entirely. All three were hit here in a single day.

Assert **which** failure fired, never that some failure did. Every
shell script runs under `set -euo pipefail`, so almost any mistake
exits non-zero and a bare non-zero assertion is satisfied by the failure
you did not mean.

## Workflows

CI is split by responsibility rather than gathered in one file:

| file | what fails there |
|---|---|
| `ci.yml` | the rustfmt / clippy / test / coverage / MSRV / package / semver gates, plus freshness audits on the scheduled workflows |
| `lint-shell.yml` | shellcheck and the shell test suite |
| `lint-powershell.yml` | PSScriptAnalyzer on `install.ps1` |
| `debian-build.yml` | the .deb builds under `debian/rules`' narrower feature set, on every change that can shift it |
| `binary-budget.yml` | the release binary fits `bench/binary-budget-mib` |
| `asset-contract.yml` | the release asset names, signing-key rotation, install self-test, and shell/PowerShell signing fixtures |
| `podman-lane.yml` | the integration suite against live Podman 5 and Podman 6 in a Fedora qemu VM, on every pull request |
| `podman-lane-develop-nightly.yml` | the same lane, instrumented for coverage, fired nightly from `develop` |
| `dco.yml`, `line-limit.yml`, `workflow-lint.yml` | one rule each |
| `main-guard.yml` | `develop-only`: only the release pull request can target `main` |
| `release.yml` | tag validity, Cargo.toml / debian/changelog / Cargo.lock agreement, `cargo audit`, signed binaries, GitHub Release |

Every reusable this repository calls lives in
`.github/workflows/reusable-*.yml`. Nothing is pulled remotely, and there is
no pin to bump: the fourteen files arrived in #1566 as byte-identical copies
of `Glyndor/.github` v1.21.0, verified file by file against
`git show v1.21.0:.github/workflows/<name>`.

**They are maintained here now, so byte-identity is provenance and not a
property to rely on.** Several already differ, and the count only grows.
`git diff` against that tag is the way to ask which, rather than a list in
this file that goes stale on the next edit. A fix that belongs upstream too
has to be made in both places by hand; that is the bill the independence was
bought with.

**Job ids are load-bearing.** A required status check is named
`<caller job id> / <inner job name>`, so renaming a job renames its check
and creates a phantom the ruleset still requires, which blocks every pull
request with no explanation. Move jobs between files freely; renaming one
is a ruleset change.

## Security

Never open a public issue for a vulnerability. Use the Security tab,
**Report a vulnerability**. The organisation's `SECURITY.md` applies here
and is deliberately not duplicated in this repository.