# Security model

This document describes podup's privilege posture, trust boundaries, and attack
surface so operators can reason about it during a security review (for example
an ATO/SSP assessment). The self-update and release trust chain is covered
separately in [self-update.md](self-update.md), and
[threat-model.md](threat-model.md) lays the same system out per threat, with the
control that answers each one, the evidence that the control works, and the
residual risks.

## Privilege posture

- podup runs entirely as the **invoking user**. It is not setuid/setgid and
  acquires no capabilities of its own.
- It drives **rootless Podman** over the libpod REST API on a Unix socket. Any
  privilege a container ends up with is granted by Podman/the kernel, bounded by
  the launching user's own privileges — a rootless container can never exceed
  them. Fields that assume more (`privileged`, `oom_kill_disable`,
  `mem_swappiness`, `cpu_rt_*`) are forwarded but warned about, since they are
  reduced or ineffective rootless.
- podup keeps **no persistent state** of its own outside the Podman objects it
  creates and a per-project advisory lock file under the user's runtime
  directory.

## Trust boundaries

| Boundary | Trusted? | Notes |
|----------|----------|-------|
| Podman socket (`PODMAN_SOCKET`/`DOCKER_HOST`) | Trusted, local-only | Whoever can reach it controls the engine; this is the primary boundary. Only `unix://`/`npipe://` are accepted — remote schemes are rejected fail-closed. |
| Compose file and its referenced files | **Trusted input** | Treated like a Makefile (see below). |
| Release artifacts (`podup update`, installer) | Untrusted transport | Verified against an embedded Ed25519 key + provenance attestation, fail-closed. |
| Container filesystem (e.g. `cp` archives) | Untrusted | Tar extraction refuses path-traversal (zip-slip) entries. |
| Network/TLS to GitHub/crates.io | Untrusted | Integrity comes from signatures, not transport. |

## Connection: the libpod socket is local-only

- The libpod socket is **strictly local**. Only a `unix://` socket path (or an
  `npipe://` named pipe on Windows) is accepted, whether it comes from
  `--socket`, `PODMAN_SOCKET`, `DOCKER_HOST`, or auto-detection. Remote schemes
  (`tcp://`, `ssh://`, `http(s)://`, `fd://`) are **rejected fail-closed** —
  podup never connects to a remote engine, so the socket boundary is always a
  local one.

## Compose files are trusted input

A compose file is treated like a Makefile: running podup on one is equivalent to
trusting its author. Path-valued keys the spec resolves relative to the compose
file (`extends.file`, `env_file`, `label_file`, `include`) may reference paths
outside the project directory, including `../`. Do **not** run podup on a compose
file from an untrusted source. `include` accepts an absolute path and uses it as
given, the same as `extends.file` and `env_file` — there is no containment here
to rely on.

## Container hardening (compose security keys)

The compose keys that constrain a container are translated onto Podman's
`SpecGenerator` and take effect on the running container — they are not silently
dropped. Everything below remains bounded by the rootless ceiling: a key can
only tighten, never widen, what the launching user already has.

A `podup audit [-f ...] [--strict]` subcommand reads the project the same way
`config` does and prints, for each service, which of those keys the file did
not set: host-binding namespacing modes, missing `read_only: true`, missing
`cap_drop: [ALL]`, missing `security_opt: [no-new-privileges:true]`,
missing `pids_limit`, missing memory limit, missing `userns_mode`, secrets in
`environment:` instead of `secrets:`, and unpinned `:latest` images. The
check list and exit codes are documented in
[commands.md](commands.md#audit); `audit` never changes what `up` does, so it
can be added to a CI gate without altering runtime behaviour.

- `security_opt` is parsed into the matching SpecGenerator fields:
  - `no-new-privileges` → `no_new_privileges`
  - `seccomp=<profile.json>` (and `seccomp=unconfined`) → `seccomp_profile_path`
  - `apparmor=<profile>` → `apparmor_profile`
  - `label=<opt>` (SELinux user/role/type/level, or `label=disable`) →
    `selinux_opts`
  - `mask=<paths>` / `unmask=<paths>` → `mask` / `unmask`
- `device_cgroup_rules` entries are parsed and applied as the container's device
  cgroup rules (a malformed entry is warned about and skipped, not fatal).
- CDI devices (Container Device Interface, e.g. `nvidia.com/gpu=all`) requested
  under `devices:` are passed through to Podman, which resolves them by name.
- Per-mount hardening — `noexec`, `nosuid` and `nodev` — is carried onto a
  volume's mount options, so a mount can deny binary execution, ignore
  setuid/setgid bits, and block device nodes. The short form spells them as raw
  mount options (`cache:/app/cache:noexec`); the long form takes them as
  booleans under `volume:`. See [Per-mount hardening
  options](docker-migration.md#per-mount-hardening-options-noexec-nosuid-nodev).

## Secret and config handling

- `secrets:`/`configs:` sourced from inline `content:` or `environment:`, and
  from a `file:` path, are created as Podman-native secrets over the libpod API
  (under a project-scoped name) and injected into the container — podup writes no
  secret material to a host directory. They are removed again on `podup down`.
- `external: true` secrets/configs are injected as Podman-native secrets
  (pre-flighted for existence), pointing at a pre-existing `podman secret`.
- A `file:` source is read at `up` time and its bytes become the secret. Nothing
  on the host is mounted, relabelled or otherwise modified. With no `mode:` given
  the secret is mounted with the host file's own permission bits, so a `0600`
  secret stays unreadable to a non-root container user.
- Because the payload is a copy taken at `up`, editing the host file afterwards
  does not reach an already-running container; recreate it to pick up a new
  value. (A rotation that replaces the file atomically — write-new-then-rename,
  which is what careful tools do — was never visible to a running container
  either, because a file bind mount pins the inode.)
- Dangerous secret file modes (setuid/setgid/sticky/executable) are rejected.
- The `config` subcommand redacts inline `content:` secrets before printing.

## Logging and information disclosure

- Default logging does not print secret values. Running with `RUST_LOG=debug`
  can emit environment variable values and resolved secret/config file paths;
  avoid debug logging where the terminal or log sink is not trusted.
- podup writes no secret material to its own persistent state.

## Memory safety

The crate forbids `unsafe` by default (`#![deny(unsafe_code)]`). The few
unavoidable FFI calls (rootless uid/gid lookups, `flock`, `stat`) are isolated,
individually justified with safety comments, and unit-tested.

## Supply chain

- Dependencies are pinned in `Cargo.lock`; `cargo deny` enforces a license
  allowlist and bans yanked crates, and `cargo audit` runs weekly in CI.
- No third-party CI actions are used — only GitHub-owned (SHA-pinned) actions.
- Releases are Ed25519-signed and carry GitHub build-provenance attestations; a
  CycloneDX SBOM and third-party license attribution are published with each
  release. See [self-update.md](self-update.md) for verification steps.
- The release reads per-asset hardening off every binary before signing, so a
  property that rests on a toolchain default is measured per binary rather than
  asserted: static PIE, full RELRO, NX stack and a stripped symbol table off
  the four Linux ELF binaries and the binary inside each `.deb`;
  `DYNAMIC_BASE`, `HIGH_ENTROPY_VA`, `NX_COMPAT` and `GUARD_CF` off each
  Windows PE's `DllCharacteristics`; `MH_PIE` in `otool -h` and a stripped
  symbol table off each Mach-O. The scripts are `.github/scripts/check-hardening.sh`
  (Linux), `.github/scripts/check-hardening.ps1` (Windows) and
  `.github/scripts/check-hardening-macos.sh` (macOS); each has its own test
  with one control binary per property.
- The Debian package can be built fully offline from a vendored crate tree, for
  air-gapped/classified environments.
- **Image signatures are the host's policy, and podup inherits it.** Every pull
  podup asks for is performed by libpod, which applies the host's
  `containers-policy.json` (system-wide in `/etc/containers/`, per user in
  `~/.config/containers/`) and the registry configuration under
  `registries.d`. A host that requires signatures for a registry gets that
  enforcement on `podup up` with nothing added on podup's side; the default
  shipped by most distributions, `insecureAcceptAnything`, enforces nothing.
  Measured on Podman 5.7.0 with a `reject` rule scoped to one repository,
  2026-09-03:
  `up` fails at the pull with libpod's own message, `Source image rejected:
  Running image docker://... is rejected by policy.`, and creates no
  container. The policy is consulted at pull time only: an image already in
  local storage runs under `pull_policy: missing` (the default) without the
  policy being asked, and `pull_policy: always` or `newer` asks it again. A
  host that wants the policy to bite on every `up` sets `pull_policy` to one
  of those two. The rule that requires sigstore signatures from one registry,
  for reference:

  ```json
  {
    "default": [{ "type": "reject" }],
    "transports": {
      "docker": {
        "registry.example.com": [{
          "type": "sigstoreSigned",
          "keyPath": "/etc/containers/keys/registry.example.com.pub",
          "signedIdentity": { "type": "matchRepository" }
        }]
      }
    }
  }
  ```
- A service declaring `x-podman-autoupdate: registry` is pulled on every
  `up` with policy `newer`, so an upstream image a registry serves today may
  not be the one an operator reviewed yesterday. It is the same risk a
  `pull_policy: always` deployment already carries, but on every command
  rather than on a manual rebuild. The signature policy above is what bounds
  that pull: a registry the host requires signatures from is checked on every
  one of them.

## Self-update

The `podup update` mechanism and its release trust chain — the embedded Ed25519
keys, the verify-before-install flow, key rotation, and independent/offline
verification — are documented in [self-update.md](self-update.md).

### Installer error classification

`install.sh` and `install.ps1` distinguish two failure modes that share the
same high-level symptom (a signature check that didn't pass) but have
different remedies:

- **Release-tampering (rc=1 / `Fail`)**: every embedded key rejected the
  signature. Treat as a release-side problem: do not retry, do not bypass.
- **Configuration problem (rc=3 / `Fail`)**: at least one embedded key was
  set but could not be decoded into a 32-byte Ed25519 point — a malformed
  `PODUP_RELEASE_PUBKEY_B64` / `PODUP_RELEASE_PUBKEY2_B64` (or, with a
  third rotation slot, `PODUP_RELEASE_PUBKEY3_B64`) override, a stray
  whitespace, a non-base64 character. The release itself may be fine; the
  user-side configuration needs correcting.

Both fail closed. The split exists so a configuration problem isn't reported
as "release may be tampered", which would push a fork maintainer to debug the
wrong side. The release-time verifier in `.github/scripts/verify-signing-key.py`
uses the same exit-code scheme.

### Per-asset .sig verification

Every detached signature in a release is verified against the keys
`install.sh` ships — not just `SHA256SUMS.sig`. Per-binary, per-deb, per-SBOM
and per-installer signatures are checked at release time (CI step "Verify every
signature against the keys consumers embed") and the installers themselves
verify the per-asset signature of whatever they fetch. A per-binary
substitution is the failure mode `SHA256SUMS` cannot catch: the manifest can be
re-signed to match the swapped binary, so trusting `SHA256SUMS.sig` alone
leaves a hole. The per-asset loop closes it.

## What podup trusts libpod to defend

Every cross-layer transition from the compose file into the libpod API is
filtered by an in-crate validator before the call is made, on the principle
that a hostile or buggy libpod must not be enough on its own to push bad
data into a process or onto disk:

- **Container, project, volume, secret, network, and config names** are
  matched against the podman object-name pattern (`[a-zA-Z0-9][a-zA-Z0-9_.-]*`)
  in `is_valid_object_name`; an invalid name is rejected client-side with a
  clear error instead of an opaque libpod HTTP 500.
- **Project names** are filtered through `is_safe_project_name` (lowercase ASCII
  letter/digit followed by lowercase letters/digits/`-`/`_`, ≤ 128 chars) at
  the dispatch boundary, so the value never reaches a code path that builds
  a filesystem path from it.
- **URL paths** go through `urlencoded` so container names, project names,
  and tags with arbitrary bytes reach libpod as a single encoded segment.
- **Quadlet values** are filtered through `escape_unit_value` /
  `safe_unit_stem`; the unit filename is checked again by `write_units` so
  a poisoned value cannot break out of the output directory.
- **Signal names** are resolved to numbers through `resolve_stop_signal` —
  the libpod endpoint is a number, a string returns HTTP 500.
- **Pull policies** are normalised through `libpod_pull_policy`; an
  unrecognised value is a hard error rather than a silent default, so a
  typo'd `pull_policy: alaways` does not flip the policy the user intended.
- **Timeouts** are parsed by `parse_timeout` and clamped; an out-of-range
  value is rejected, not silently zeroed.
- **Self-update bytes** are verified against an Ed25519 signature
  (`RELEASE_PUBKEYS`) plus a SHA-256 manifest match, then installed through
  an `O_NOFOLLOW` rename with the running binary's mode preserved. The
  staged binary's `--version` is checked against the resolved release tag
  before the in-place swap.

## What podup does not defend

podup is not a sandbox. The points below are **documented gaps** an operator
or auditor must know about, and they are not changed by tightening the
filters above. They are not flaws; they are the conscious boundary.

- **A compose file that asks for a privilege podup has no policy on.** A
  `cap_add: [SYS_ADMIN]`, `pid: host`, `network_mode: host`, or
  `runtime: /path/to/binary` is forwarded to libpod as-is. podup emits a
  `tracing::warn!` per active host-binding / privilege-escalation mode
  (`network_mode: host`, `privileged: true`, `pid`/`ipc`/`uts`/`cgroup`/`userns_mode: host`,
  and the `container:<id>` namespace-sharing form) and the `config`
  command surfaces the same modes at default log level. The actual gate
  is libpod's own validator; podup does not second-guess it.
- **Compose-sourced paths are unconfined by design.** `label_file`,
  `env_file`, `extends.file`, `include`, `secrets.file`, `build.context`,
  and the bind sources in `volumes:` accept `../` and absolute paths; the
  spec treats them as trusted operator input. The operator who runs
  `podup` on a compose file is the one choosing to honour those paths —
  the same posture a `Makefile` has.
- **Inline-secret `file:` sources have a point-in-time lifetime.** The
  bytes are read at `up`, copied into a Podman-native secret, and persist
  until the next `up` overwrites. Editing the file on disk after `up`
  does not propagate; recreate the container to pick up the new value.
- **The static-review sweep surfaced real limits that podup cannot fully
  close by itself.** Whether libpod's per-field validators actually reject
  every value podup forwards is verifiable only by running against a real
  Podman; whether the live streaming endpoints are reachable in production
  is verifiable only by fuzzing and by the live integration lane (see
  below). podup keeps both as required checks; it does not claim the
  check is exhaustive.

## What the live integration lane validates

The `podman-lane` workflow boots Fedora qemu VMs in the runner (nested-virt —
`ubuntu-24.04` exposes `/dev/kvm`) with full systemd, once per supported
Podman major (Fedora 44 for Podman 5, rawhide for Podman 6). Each VM runs
the integration suite as a rootless user. The lane is a **required status
check** on every pull request; the nightly schedule and the lane-internal
`PODUP_REQUIRE_PODMAN=1` together close the "no engine, all tests skip"
green path the unit suite can otherwise open. The per-major
`.github/podman-known-failures-<major>` files are a **classification** of
the failures the lane still sees, not a count; any test that fails without
appearing on its major's list is reported as an unexpected regression.

The lane's `--test-threads` cap is set to the largest value that keeps the
connection-drop noise the nested-virt transport adds under control; bumping
it costs roughly twenty minutes per leg per run. The suite's per-test
identity (not just the pass count) is what tells a regression from a
flaky drop, and the lane's retry loop covers exactly the drop signatures
the transport produces.

## What static review cannot tell us

A static reviewer reads source. Several questions raised by the 2026
audit of podup are not decidable from source alone:

- Whether libpod's per-field validators actually reject everything podup
  forwards. The integration lane (above) is the answer, and it is run
  against every supported Podman major on every engine change.
- Whether the rollback attack via CDN re-sign of an older release would
  be accepted by a field binary. The PoC for that is in
  `install.sh:verify_version_self_test` and `install.ps1:Test-StagedVersion`
  — the staged binary's `--version` is checked against the resolved
  release tag before the in-place swap, so a CDN that hands back a
  legitimately-signed older release is rejected.
- Whether the race conditions in `cp_to_container`, `run_attached`, and
  the streaming endpoints are reachable in production. Fuzz targets
  (`fuzz/fuzz_targets/`) and the live-Podman lane are the only ways to
  confirm.

## Reporting

Report vulnerabilities privately via the repository's **Security tab → Report a
vulnerability** (never a public issue). See the organization
[security policy](https://github.com/Glyndor/.github/blob/main/SECURITY.md) for
response targets.
