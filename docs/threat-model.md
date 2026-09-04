# Threat model

What podup protects, against whom, with which control, and how each control is
known to work. [security-model.md](security-model.md) describes the posture and
the boundaries; this page is the assessor's view of the same system: one row per
threat, the control that answers it, and the evidence that the control is real
rather than a sentence. Residual risks are listed last, on purpose, because a
threat model that ends with "everything is covered" has not been written
honestly.

Evidence is of three kinds, and the column says which: a **test** is a case in
the repository that fails when the control is removed; a **gate** is a required
status check that blocks a merge; a **measurement** is a number read off a real
run, with the date. A control with none of the three is listed as such.

## Assets

| Asset | Where it lives | Why it matters |
|---|---|---|
| The operator's Podman engine | the libpod socket, local to the machine | whoever drives it runs containers as the operator |
| Compose files and the files they reference | the operator's project directory | podup executes what they describe, as a Makefile would |
| Secrets and configs | Podman-native secrets, created per project | injected into containers; never written to a host directory by podup |
| The release binaries and packages | GitHub releases, the apt archive, the Homebrew tap, the Scoop bucket | what every install and every self-update runs |
| The release signing key | the organization's CI secret; public half embedded in the binary, the installers and the channels | the trust anchor for everything above |
| The container's writable layer | Podman storage | destroyed on a recreate; the operator must be told when that happens |

## Adversaries considered

- **A network position between the operator and GitHub or a CDN**: can serve any bytes, including an older, legitimately signed release.
- **A compromised upstream crate or tool** pulled into a build or a release job.
- **A compromised release of a sibling product** sharing the organization's release key.
- **A hostile or buggy libpod** answering the socket with unexpected shapes.
- **A hostile compose file** the operator chose to run.
- **A local process on the same machine** without the operator's privileges.

Not considered, and said so: an adversary who already holds the operator's account or the libpod socket, an adversary who holds the release signing key, and physical access. podup is not a sandbox and does not claim to contain any of these.

## Threats and controls

### Supply chain of what the operator installs

| Threat | Control | Evidence |
|---|---|---|
| A release asset replaced on the wire or on a mirror | every published asset is Ed25519-signed; `install.sh`, `install.ps1` and `podup update` verify the signature against the keys embedded in them before anything is written, and fail closed | test: `tests/fixtures/releases/` drives both installers through a deterministic three-slot fixture in `asset-contract.yml`; gate: `asset-contract` on every pull request |
| `SHA256SUMS` re-signed to match one swapped binary | every per-asset `.sig` is verified at release time against the keys the installers embed, not only `SHA256SUMS.sig` | gate: the release workflow step "Verify every signature against the keys consumers embed" |
| A CDN hands back an older, legitimately signed release (rollback) | the staged binary's `--version` is compared against the resolved release tag before the in-place swap | test: `install.sh:verify_version_self_test`, `install.ps1:Test-StagedVersion` |
| The embedded public key and the signing key drift apart | a release-time check verifies a real signature against the constant the installers ship; a regression test pins the embedded key against a real published `SHA256SUMS` | test: `embedded_key_verifies_real_release`; gate: `verify-signing-key.py` in the release workflow |
| A signing-key rotation strands installed clients | two key slots, make-before-break: a transition release trusts both keys and is signed with the old one, the next is signed with the new one | test: the three-slot fixture above exercises the rotation slot; the procedure is in [self-update.md](self-update.md) |
| A build script in a dependency runs beside the signing key | the jobs that hold the key install Python by hash only (`pip install --require-hashes`); a job that installs third-party tooling any other way must hold no secret | gate: `workflow-lint`'s tooling-isolation assertion, on every pull request; test: the assertion has a behaviour suite in the distribution channels that carry the same file |
| A vulnerable or yanked crate ships | `cargo audit` and `cargo deny` on every lockfile change and weekly; the release refuses to build on a finding; the fuzz workspace lockfile is audited too | gate: `audit / cargo audit`, `audit / cargo deny`; measurement: the weekly schedule is watched by `freshness-audit`, which fails when the cron stops |
| A third-party GitHub Action changes under a tag | no third-party actions; the runner primitives used are pinned to a commit SHA with the version beside it | gate: `workflow-lint` (actionlint plus the organization's rules) |
| A dependency bump goes unreviewed | Dependabot proposes every bump; a silent Dependabot is detected, and the detector distinguishes "nothing to bump" from "dead" by asking upstream for newer tags | gate: `dependabot-freshness` on every pull request; test: 34 cases in the channels that carry the same reusable |
| The Debian package built in a moving base image acquires a glibc floor nobody chose | the Linux release binaries are static musl; the `.deb` follows the same target | measurement: `static-pie linked`, zero `GLIBC_*` symbols on the published binary, 2026-08-30 |
| A Linux binary ships without the hardening its target is assumed to give it | before anything is signed, the release reads static PIE, full RELRO, a non-executable stack and no symbol table off the two Linux binaries and the binary inside each `.deb`, and fails on any one missing; a pull-request job builds both targets on their own runners, runs the binary once and reads the same properties; every build leg runs its binary once before signing | test: `tests/shell/check-hardening.test.sh`, one control binary per property, built one linker flag away from the good one; measurement: on 5.7.0 the x86_64 binary had all four and the arm64 binary was a plain static executable at a fixed address, since rustc links the aarch64 musl target that way by default; every arm64 Linux release before this row shipped without ASLR, and `.cargo/config.toml` is what changed it, 2026-09-02 |
| A Windows binary ships without the hardening its target is assumed to give it | before anything is signed, the release reads `DllCharacteristics` off each Windows binary and requires `DYNAMIC_BASE` (ASLR), `HIGH_ENTROPY_VA`, `NX_COMPAT` (DEP) and `GUARD_CF` (Control Flow Guard); rustc emits the first three on MSVC by default, `GUARD_CF` is opt-in (`-C control-flow-guard`) and is enabled for both MSVC targets in `.cargo/config.toml`, the way the arm64 musl link is; the script parses the PE header itself, with no Visual Studio tooling | test: `tests/powershell/check-hardening.Tests.ps1`, one control binary per property, built by clearing one bit of `DllCharacteristics` in a copy of a good PE; both Windows legs run the gate before signing, on the runner that built the binary; reading the PE needs no host the binary targets, so the gate would hold on a cross-compile runner too |
| A macOS binary ships without the hardening its target is assumed to give it | before anything is signed, the release reads `MH_PIE` off `otool -h` and confirms no local symbols remain (`nm` shows only `U`) for each Mach-O binary; rustc emits PIE on Apple targets by default and the release profile strips local symbols, so a missing property is a toolchain or linker regression the release refuses to publish | test: `tests/shell/check-hardening-macos.test.sh`, one control per property built with `clang` (`-Wl,-no_pie` for the PIE control, an unstripped link for the other); the script runs on both macOS legs of the release; the test skips where `clang`/`otool` are absent, so a macOS job in `lint-shell.yml` runs it on a Mac and then reads both properties off a binary built the way the release builds; the first release run of the script (v5.9.0, 2026-09-04) failed both darwin legs on an `otool` parsing bug that this job would have caught |
| An image is replaced at the registry, or pulled from a registry nobody vetted | libpod applies the host's `containers-policy.json` to every pull podup requests; a `reject` or `sigstoreSigned` rule for a registry refuses the pull before any container exists, and podup surfaces libpod's message. The policy is the host's, not podup's, and it is consulted at pull time only | measurement: Podman 5.7.0, `reject` scoped to one repository, `up` fails with `Source image rejected: Running image docker://... is rejected by policy.` and creates nothing; the same image already in local storage runs under `pull_policy: missing` and is refused again under `always`, 2026-09-03 |

### The engine boundary

| Threat | Control | Evidence |
|---|---|---|
| podup is pointed at a remote engine and secrets leave the machine | only `unix://` and `npipe://` are accepted, from every source of the socket path; remote schemes are rejected before a connection | test: unit tests on the socket resolver |
| A project in a pod puts every service in one network namespace, so a compromised service reaches its siblings on `localhost` | pod mode is opt-in (`x-podman-pod: true`), only the network namespace is shared, and the doc names the consequence; a project that wants network isolation between services keeps the default, one container per service on the project network. What the pod adds is one namespace to audit and one place ports are published | test: the refusals and the pod request are unit-tested against the fake engine; measurement: `podman pod inspect` on a project started with the key shows `SharedNamespaces: [net]` and every service in `Containers`, 2026-09-03 |
| libpod returns a name or path that reaches the filesystem | object names are validated against Podman's own pattern before use; project names are filtered at the dispatch boundary; Quadlet values are escaped and the unit filename re-checked before writing | test: `project_name_safety.rs`, `quadlet.rs`, the `names` and `unit` unit tests |
| A container archive escapes its destination on `cp` | tar extraction refuses path-traversal entries; the destination entry is compared against what was uploaded | test: `cp_flags.rs`, `copy` unit tests; measurement: the Podman 6 `cp` defect was diagnosed and fixed on a real guest (#1097) |
| libpod reports a failed pull on a `200` and podup starts a stale image | in-band `error` lines are read and surfaced; presence is verified after a pull | test: `pull` unit tests, `pull_ignore_failures_continues_past_bad_image` |
| A recreate destroys a writable layer without telling the operator | a replaced container is reported as `Recreating`/`Recreated`, never as `Starting`; a container is replaced only when its config or its image changed | test: `recreate_vocabulary.rs`, `recreate_on_image.rs`, each shown red with its control reverted |

### Secrets and configs

| Threat | Control | Evidence |
|---|---|---|
| A secret is written to the host | every source, including `file:`, becomes a Podman-native secret; nothing is mounted from or written to a host directory | test: `secret_safety.rs`, the `secrets` unit tests |
| A secret value appears in an error or a log | the payload type has no `Display` and no `Debug` that prints it; `config` redacts inline content | test: `secret_safety.rs` asserts the redaction and the absence of the bytes in every error path it can reach |
| A build secret is baked into an image layer | build secrets are excluded from the build context in both context-tar builders | test: the build context unit tests |
| A `file:` secret with a dangerous mode is injected | setuid, setgid, sticky and executable modes are rejected | test: the `secrets/plan` unit tests |

### The host

| Threat | Control | Evidence |
|---|---|---|
| A systemd unit written by `autostart` injects a directive through a path | control characters are rejected in every value that lands in a unit; the unit stem is sanitised and re-checked before write | test: `autostart` unit tests |
| Two podup invocations race on one project | a per-project advisory lock under the runtime directory | test: `lock` unit tests |
| A staged self-update is swapped through a symlink | the staging file is created `O_EXCL`, the target is replaced through an `O_NOFOLLOW` rename with its mode preserved | test: `install_binary` unit tests |
| Debug logging leaks environment values | documented: `RUST_LOG=debug` can print them; default logging does not | no test; the boundary is stated in [security-model.md](security-model.md) |

## How the evidence is kept honest

Reading a test does not say whether it works. The controls above were, where
marked as tests, checked by deleting the control and watching the test go red;
several tests in this repository were rewritten after they stayed green with
their control gone (#1514 is one). The organization's testing standard makes
that the rule, and the pull requests that add a control carry the run.

The unit suite runs without an engine and skips what needs one. That green path
is closed by the live lane: on every pull request that can change runtime
behaviour, a Fedora VM per supported Podman major runs the integration suite as
a rootless user, and the `Supported Podman majors` check is required. A pull
request the VM cannot observe (prose, templates) still reports the check and
boots nothing.

Coverage is measured over production lines only, since 2026-09-02: unit tests
live in sibling files so their bodies do not count as covered code, and the
threshold is the number CI measured, written beside the input.

## Residual risks

Listed so an assessor does not have to find them.

- **One maintainer.** Every pull request is reviewed and merged by the same
  person who wrote it. A required status check is matched by name, so a
  maintainer with write access could, in one pull request, replace a gate with
  a job of the same name. The organization accepts this while it is
  single-maintainer and has written down the mitigation to apply the day a
  second human gets write access: required review from code owners on the
  workflow directory.
- **The signing key is shared across the organization's products.** A
  compromised release of a sibling product could sign a package that claims
  to be podup. The apt archive binds each package to the product that
  released it and refuses a mismatch; the Homebrew tap and the Scoop bucket
  read only podup's own releases. A direct download from a sibling's release
  page has no such binding.
- **The signature implementation is not a validated cryptographic module.**
  Ed25519 is an approved algorithm; the libraries that implement it here have
  not been through a validation programme. An assessment that requires one
  needs either a validated module in the verification path or a documented
  exception.
- **No independent audit.** Every measurement on this page was made by the
  project. The static-review sweeps that shaped it are recorded in the pull
  requests they produced, and none was made by a third party.
- **Compose files are trusted.** A hostile compose file can bind any host path
  and request any privilege the operator holds. This is the documented
  posture, not a gap to close: containment of the compose author is outside
  what a compose runner can offer.
- **The live lane runs one thread per Podman major.** The cap is the largest
  value that avoids the nested-virtualisation transport dropping connections;
  concurrency defects that need parallel load are exercised only by the
  fuzz targets and by operators.

## Reporting

Report vulnerabilities privately through the repository's **Security tab**.
The organization's [security policy](https://github.com/Glyndor/.github/blob/main/SECURITY.md)
carries the response targets.
