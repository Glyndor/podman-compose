# Security model

This document describes podup's privilege posture, trust boundaries, and attack
surface so operators can reason about it during a security review (for example
an ATO/SSP assessment). The self-update and release trust chain is covered
separately in [self-update.md](self-update.md).

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
file from an untrusted source. (`include` still rejects absolute paths as
non-portable, but this is hardening, not a security guarantee.)

## Container hardening (compose security keys)

The compose keys that constrain a container are translated onto Podman's
`SpecGenerator` and take effect on the running container — they are not silently
dropped. Everything below remains bounded by the rootless ceiling: a key can
only tighten, never widen, what the launching user already has.

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
- The Debian package can be built fully offline from a vendored crate tree, for
  air-gapped/classified environments.

## Self-update

The `podup update` mechanism and its release trust chain — the embedded Ed25519
keys, the verify-before-install flow, key rotation, and independent/offline
verification — are documented in [self-update.md](self-update.md).

## Reporting

Report vulnerabilities privately via the repository's **Security tab → Report a
vulnerability** (never a public issue). See the organization
[security policy](https://github.com/Glyndor/.github/blob/main/SECURITY.md) for
response targets.
