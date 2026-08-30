# podup

docker-compose translator and runner for rootless Podman. Reads a
docker-compose file, translates it to the native libpod REST API, and manages
the container lifecycle (`up`/`down`/`logs`/`exec`/…). A single static Rust
binary, with no daemon and no Python runtime.

[![CI](https://github.com/Glyndor/podup/actions/workflows/ci.yml/badge.svg)](https://github.com/Glyndor/podup/actions/workflows/ci.yml)

MSRV 1.85 · License: MIT

<img src="docs/assets/podup-demo.gif" alt="podup running a compose stack on rootless Podman" width="760">

## Install

```sh
curl -fsSL https://apt.glyndor.net/install/podup | sudo sh
```

That is the whole install on Debian and Ubuntu. It registers the signed Glyndor
apt repository, verifies the archive key's fingerprint before trusting it, and
installs podup with apt — so upgrades and signing-key renewals arrive through
`apt upgrade` like any other package. Root is needed because it installs
packages. It leaves nothing of its own behind: the download is removed, and so
is anything it had to install just to check the key.

podup needs **Podman ≥ 5.0** (rootless). The package depends on it, so apt
installs it alongside podup, and refuses the install on a distribution whose
Podman is older than that. It also depends on `unattended-upgrades`, because an
apt-installed podup updates through apt and nothing else: `podup update` refuses
to replace a dpkg-owned binary, and only the latest release is supported.

That dependency guarantees `unattended-upgrades` is installed, not that it is
running. What switches it on is `/etc/apt/apt.conf.d/20auto-upgrades`, which is
system-wide policy for every package on the machine rather than podup's to set,
so no podup maintainer script writes it. The one-line installer above writes it
when it is absent, and Ubuntu normally has it already. If you registered the
archive yourself and then ran `apt install podup` on Debian, podup is installed
and the Glyndor archive is allowlisted, but nothing upgrades it until you run
`apt upgrade`. `systemctl status unattended-upgrades` says which of the two you
have.

Podman is daemonless, but podup speaks the libpod API, so the
socket still has to be listening:

```sh
systemctl --user enable --now podman.socket
```

### Optional — macOS

```sh
brew install glyndor/tap/podup
```

### Optional — Windows

```powershell
scoop bucket add glyndor https://github.com/Glyndor/scoop-bucket
scoop install podup
```

Scoop clones the bucket with git, so git has to be installed first — Scoop's own
installer does not bring it.

### Optional — Linux without apt

```sh
curl -fsSL https://glyndor.net/podup/install/unix | bash
```

Installs the release binary rather than a package. Use it on a distribution apt
does not serve; on Debian and Ubuntu the line at the top is better, because apt
keeps podup current and this does not.

<details>
<summary><b>Build from source · self-update · Podman versions · platforms</b></summary>

### Build from source

```sh
cargo build --release
```

### Self-update

Only for installs that did not come from a package manager — the apt build omits
it, since apt owns upgrades there.

```sh
podup update            # download and install the latest signed release
podup update --check    # report whether a newer release exists, install nothing
```

`podup update` replaces the running binary in place only after verifying the
release's Ed25519 signature and SHA-256 checksum, failing closed otherwise. See
[docs/self-update.md](docs/self-update.md) for the trust model.

### Podman version

podup tracks the **latest stable Podman** and supports its **last two majors,
Podman 5.x and 6.x**. It talks to Podman's native libpod API, requesting the
`/v5.0.0/libpod` path that Podman 6 still serves; the gate is the major version
the engine reports, so it needs **Podman ≥ 5.0**. When a new major ships, it is
added and the oldest is dropped — but only once the **newest LTS of each
distribution family carries the new one or better**, so nobody on a current
release is stranded. Both supported majors run the
integration suite in CI on every engine change (Fedora 44 for the latest 5.x,
rawhide for 6.x). Many distributions still ship 4.x, so `podman --version` is
worth checking before installing — and a distribution never changes its Podman
major version mid-release, so an LTS that shipped below the floor stays below
it for its whole supported life.

| distribution                  | Podman | podup runs |
|-------------------------------|--------|------------|
| Debian 12 bookworm            | 4.3.1  | no         |
| Debian 13 trixie              | 5.4.2  | yes        |
| Ubuntu 22.04 LTS              | 3.4.4  | no         |
| Ubuntu 24.04 LTS              | 4.9.3  | no         |
| Ubuntu 26.04 LTS              | 5.7.0  | yes        |
| Fedora 42 and newer           | 5.x+   | yes        |

Ubuntu 24.04 LTS is not supported: it ships Podman 4.9.3 and will keep shipping
that for its whole supported life. On any row marked "no", `apt install podup`
refuses rather than installing a podup that cannot reach an engine, and the
engine has to come from somewhere other than the distribution:
<https://podman.io/docs/installation>.

Driving a **remote** Podman, or a `podman machine`, is the case apt cannot
express: a package relationship only sees the local machine. Use the release
binary there. `install.sh` warns instead of refusing when no local Podman is
present, and takes `--skip-podman-check` when a local one is present but is not
the engine podup will use.

### Platforms

Linux, macOS and Windows (x86_64 and arm64). On macOS and Windows podup talks to
the `podman machine` VM through its host-side `unix://` socket or `npipe://`
named pipe; the socket must be local (remote `tcp://`/`ssh://` are rejected).

</details>

## Quick start

```bash
podup up -d      # start the stack in the current directory
podup ps         # see what's running
podup down -v    # tear down and remove volumes
```

Full command reference: [docs/commands.md](docs/commands.md).

## Design

Rootless-native libpod API, real compose-spec support (`extends`, profiles,
`develop.watch`, inline secrets), and systemd Quadlet export. There is a library
target, and the integration tests are built against it, but podup is distributed
as a binary: it is not published to crates.io and carries no semver promise about
its Rust API.

```mermaid
sequenceDiagram
    autonumber
    participant Y as docker-compose.yml
    participant P as podup
    participant L as Podman · libpod REST
    Y->>P: parse · substitute · resolve depends_on
    P->>L: create networks · volumes · secrets
    P->>L: start containers in order
    L-->>P: health / status
    P-->>Y: stack up
```

## Benchmarks

Peak memory and per-operation latency against docker-compose and podman-compose,
**all three driving the same rootless Podman**, same digest-pinned images,
median of 10 measured runs (12 iterations, 2 warm-up discarded), on podup 3.4.1.
podup is fastest in all 29 measured rows, though three teardown rows —
`multi-healthcheck`, `scale` and `many-services` — win by less than their own
standard deviation and should be read as ties; the widest gaps are the ones
with many services.

| | podup | docker-compose | podman-compose |
|---|---|---|---|
| memory per command | **8.1 MiB** | 29.8 MiB | 52.9 MiB |
| `up`, 42 services | **1.13 s** | 1.57 s | 9.67 s |
| `up`, 12 services | **0.39 s** | 0.51 s | 2.92 s |
| `config` (parse only) | **9.4 ms** | 48.8 ms | 108.1 ms |

<img src="docs/assets/bench.svg" alt="Bar chart: podup uses about 8 MiB per command against 30 MiB for docker-compose and 53 MiB for podman-compose, and is faster in every measured scenario" width="760">

Full tables and methodology: [docs/benchmarks.md](docs/benchmarks.md).

## Documentation

- [Commands](docs/commands.md)
- [Migrating from Compose](docs/docker-migration.md)
- [Autostart at boot](docs/autostart.md)
- [Benchmarks](docs/benchmarks.md)
- [Self-update](docs/self-update.md)
- [Security model](docs/security-model.md)
- [Debian packaging](docs/debian-packaging.md)

## License

[MIT](LICENSE). Report vulnerabilities privately via the **Security** tab, never in a public issue.
