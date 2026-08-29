# Debian packaging

podup is distributed for Debian and Ubuntu through the self-hosted signed apt
repository at [`apt.glyndor.net`](https://apt.glyndor.net) — that is the
supported path, described below. Inclusion in the official Debian/Ubuntu
archives is explicitly **not** a goal. The `debian/` directory in this
repository builds the `.deb` that the apt repository serves; this page covers
how to build it and how it is published.

## Build a .deb locally

```bash
dpkg-buildpackage -us -uc -b
```

Requires `debhelper`, `cargo` and `rustc >= 1.85` (the crate's declared
`rust-version`). The package installs `/usr/bin/podup` and the `podup(1)` man
page.

The packaged binary is built with the self-update feature **compiled out**:
`debian/rules` builds with `--no-default-features --features watch,completions`,
dropping the `update` feature (and its `ureq` + TLS + Ed25519 stack). This is
why `podup update` on a deb-installed binary refuses outright and points back to
the package manager — the capability is absent from the binary, not merely
gated at runtime by a dpkg-ownership check.

## Prebuilt .deb from releases

Each tagged release attaches a signed `.deb` per architecture —
`podup_<version>_amd64.deb` and `podup_<version>_arm64.deb` (each with its
`.sig`, and an entry in the release `SHA256SUMS`). Each architecture is built
natively in its own `debian:sid` container (amd64 on an x64 runner, arm64 on an
arm64 runner — no emulation). Install the one matching your architecture
directly:

```bash
sudo apt install ./podup_<version>_amd64.deb   # or _arm64.deb on aarch64
```

`podup update` refuses to self-replace a binary installed this way — it would
desync dpkg's record of the file — and points back to the package manager;
upgrade with `apt` instead.

## apt repository (apt.glyndor.net)

For Debian and Ubuntu (amd64 and arm64), podup is served from the **Glyndor apt
repository** at `https://apt.glyndor.net`, alongside other Glyndor packages.
The repository lives in its own repo, [Glyndor/apt](https://github.com/Glyndor/apt):
a workflow there downloads the latest `.deb` release asset of each tracked
product for every served architecture (amd64 and arm64), verifies each against
the release signing key, builds a signed `reprepro` repository, and publishes
it to Cloudflare R2 behind `apt.glyndor.net`. It is rebuilt fresh each run, so
it always carries the current version of every package (no old-version
support). podup's only responsibility is to attach a
`podup_<version>_<arch>.deb` asset (amd64 and arm64) to each release — which
the `build-deb` matrix in `release.yml` already does.

### One-line setup

```bash
curl -fsSL https://glyndor.net/podup/install/unix | bash -s -- --apt
```

This downloads `glyndor-archive-keyring.deb` over HTTPS, installs it (registering
the signing key and source list), then runs `apt install podup`.

### Manual setup

```bash
curl -fsSLO https://apt.glyndor.net/glyndor-archive-keyring.deb
dpkg-deb -x glyndor-archive-keyring.deb keyring-check
gpg --show-keys keyring-check/usr/share/keyrings/glyndor.gpg
```

Check the printed fingerprint against the one published in the
[apt repository README](https://github.com/Glyndor/apt#verify-the-signing-key)
— a channel independent of `apt.glyndor.net`. Only once it matches:

```bash
sudo dpkg -i glyndor-archive-keyring.deb
sudo apt update && sudo apt install podup
```

`dpkg -i` runs the package's maintainer scripts as root, which is why the
fingerprint is read from a copy unpacked with `dpkg-deb -x` rather than from
the installed keyring: `dpkg-deb -x` runs nothing from the package, so the
check happens before anything in it can execute.

### Why key renewal is automatic

The signing key ships as the `glyndor-archive-keyring` package, so apt owns the
key file. When the key is rotated or its expiry extended, a new keyring version
is published and `apt upgrade` installs it — nothing for users to re-run.

> **Debian compatibility note:** the MSRV is 1.85, which Debian trixie ships, so
> trixie and sid can both build the package.

## What the skeleton covers

- `debian/control` — source/binary stanzas, build dependencies, `Depends: podman (>= 5.0)`
- `debian/rules` — debhelper with cargo overrides, `--locked` release build, tests run during the build
- `debian/podup.1` + `debian/podup.manpages` — the man page, installed by `dh_installman`
- `debian/copyright` — DEP-5, MIT
- Source format `3.0 (native)` — the repository is upstream

## Not the official Debian/Ubuntu archive

Uploading podup to the official Debian or Ubuntu archive is **not** a goal. The
self-hosted `apt.glyndor.net` repository above is the supported distribution
channel: it gives Debian/Ubuntu users `apt`-managed installs and upgrades
without the archive's process overhead (an ITP bug, a Debian Developer sponsor,
and fully offline `debcargo`/vendored builds), and it stays in lockstep with
each release on its own schedule.

The packaging mechanics carry over regardless of channel:

- **SemVer discipline**, in force since `1.0.0`: the CLI surface is stable and
  breaking changes wait for a major bump.
- **crates.io** — the crate metadata is in place (`cargo package` verifies
  clean) for publishing podup as a library; this is independent of the `.deb`.

## Versioning

`debian/changelog` tracks the upstream version (native package, no `-1`
revision), and is bumped with each release so `dpkg-buildpackage` stamps the
`.deb` with the matching version.
