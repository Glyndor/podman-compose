## Summary

<!-- What does this PR do? 1-3 bullet points. -->

## Changes

<!-- List the main changes made. -->

## Test plan

<!-- How was this tested? Check all that apply. -->

- [ ] `cargo fmt --all --check` is clean
- [ ] `cargo clippy --locked --all-targets --all-features -- -D warnings` is clean
- [ ] `cargo test --locked --all-features --workspace` passes locally
- [ ] `./tests/shell/*.test.sh` pass locally (every tracked test, including `ci-runs-every-test.test.sh`)
- [ ] shellcheck is clean on every tracked `*.sh` and every extensionless script with a shell shebang
- [ ] Podman lane ran (live Podman 5 and Podman 6, in the PR's own check run) for changes to `internal/`, the engine integration, or the libpod REST adapter
- [ ] Asset-contract fixtures ran for changes to `install.sh`, `install.ps1`, `internal/update/install.rs`, the signing scripts or the release fixtures

<!--
A test that was not watched fail is not a test. If this PR adds or changes
a check, say which control you removed to make it go red, and what it
reported. See standards/testing, "Three ways a sabotage lies to you".
-->

- [ ] New or changed checks were verified by deleting the control and watching them fail

## Checklist

- [ ] Targets `develop` (the release pull request that targets `main` is the one exception, gated by `reusable-main-guard.yml`)
- [ ] Commits are signed off (DCO, `git commit -s`)
- [ ] Commits are signed (`required_signatures` is enforced on `develop` and `main`)
- [ ] Labels applied (`type:`, `prio:`, `effort:`, `area:` where applicable)
- [ ] Every new file in `tests/shell/` is named in some workflow's `test-command` (`ci-runs-every-test.test.sh` enforces this on every pull request)
- [ ] No secrets, keys or credentials in code, logs or fixtures
- [ ] Docs updated if behaviour changed (`docs/commands.md`, `docs/security-model.md`, `docs/self-update.md` as relevant)
- [ ] `Cargo.toml` and `debian/changelog` are at the same version (the release gate compares them, but a mismatched pair during development is a hidden cost)
- [ ] `cargo build --release --locked --bin podup` fits `bench/binary-budget-mib` for changes that can move the binary size

## Related issues

<!-- Closes #123, refs #456 -->

The `Closes #N` trailer does not auto-close on pull requests into
`develop`, because the default branch is `main` and GitHub only closes
from there. Mention the issue in this body and update the issue's
labels; closing happens when the release pull request merges into
`main`.