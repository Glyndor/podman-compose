# Self-update security model

`podup update` replaces the running binary with the latest release. The default
run resolves the newest release, verifies it, and installs it; `--check` reports
whether a newer release exists and then stops, downloading nothing and never
touching the installed binary; `--force` reinstalls the latest release even when
it is not newer than the current build. The design goal is that **the update
source is impossible to tamper with**: no attacker — even one who controls the
network or the download host — can make `podup` install a modified binary.

## Trust anchor

The trust anchor is **not** the download domain, DNS, or TLS. It is the set of
**Ed25519 public keys compiled into the binary** (`internal/update/verify.rs`,
`RELEASE_PUBKEYS`). The matching private key is held only as a CI secret and
signs every release in CI (`.github/workflows/release.yml`).

Because the public key is baked into a binary that is itself signed and carries
a GitHub build-provenance attestation, an attacker cannot swap the key without
invalidating the binary that contains it. TLS only protects transport; it is not
relied on for integrity.

## Verification flow

`podup update` performs, in order, failing closed at the first problem:

1. Resolve the latest release tag from the GitHub API and compare it with the
   compiled-in version (`env!("CARGO_PKG_VERSION")`). Nothing is downloaded if
   the current build is already newest (unless `--force`), and `--check` returns
   here without downloading anything.
2. **Refuse a package-manager-managed binary.** If the running executable is
   owned by a system package manager (e.g. installed from the `.deb`, detected
   via `dpkg-query`), `podup update` refuses **before downloading anything** and
   redirects you to the package manager (e.g. `apt upgrade podup`); overwriting
   the file in place would desync the manager's records. This applies even with
   `--force`. cargo-install / manual layouts (`~/.cargo/bin`, `/usr/local/bin`)
   are not package-owned and update normally.
3. Fetch `SHA256SUMS` and `SHA256SUMS.sig` and **verify the Ed25519 signature**
   of `SHA256SUMS` against the embedded public key — *before* the binary is
   downloaded, so a tampered or unsigned release is rejected without first
   buffering a large attacker-controlled payload. A missing/placeholder key,
   malformed signature, or bad signature aborts here. The manifest and signature
   are fetched over HTTPS (rustls + webpki roots; HTTPS-only URLs).
4. Look up the binary's expected SHA-256 in the now-trusted `SHA256SUMS`,
   download the platform binary over HTTPS, and verify its bytes match (the
   digest comparison runs in constant time).
5. Atomically replace the running executable (write a sibling temp file with
   `O_EXCL`/`O_NOFOLLOW` at mode `0600`, copy the target's mode while stripping
   any setuid/setgid/sticky bits, fsync, then `rename`). On Windows the in-use
   `.exe` is renamed aside first and cleaned up on the next run.

Any failure exits with code `3` (distinct from clap's `2` for usage errors and
from a generic `1`) and leaves the installed binary untouched.

## install.sh

The one-line installer applies the same fail-closed policy. With a release
public key configured (the default — the key ships embedded in the script), the
**Ed25519 signature check is mandatory**: it requires `python3` with the
`cryptography` package and refuses to install if the check cannot run, so the
pinned key is never silently bypassed. The GitHub build-provenance attestation
(`gh attestation verify`, pinned to the release workflow) runs as
defence-in-depth alongside it, and serves as the trust anchor only when no
release public key is configured. There is **no opt-out**: a checksum alone is
not a trust anchor. The `--apt` path likewise verifies the keyring package's
Ed25519 signature before installing it as root.

`install.sh` and `install.ps1` are themselves listed in the signed `SHA256SUMS`
manifest and carry their own `install.sh.sig` / `install.ps1.sig`, so a user
pinning a version can verify the script before piping it to a shell — the script
is no longer the one unverifiable link in the chain.

The embedded Python in both installers classifies a verification failure into
two distinct exit codes, so the calling shell can report the right problem to
the user:

- **rc=1 (signature mismatch, `Fail`)** — every embedded key rejected the
  signature. Treat as a release-tampering problem; do not retry.
- **rc=3 (key malformed, `Fail`)** — at least one embedded key was set but
  could not be decoded into a 32-byte Ed25519 point. This is a user-side
  configuration problem (a bad `PODUP_RELEASE_PUBKEY_B64` /
  `PODUP_RELEASE_PUBKEY2_B64` override, a stray whitespace, a non-base64
  character), not a release-tampering problem — the release itself may be
  fine. The CLI distinguishes them so a fork maintainer debugging a
  configuration slip isn't told to chase a phantom signature mismatch.
  `install.sh` maps rc=3 to its own `return 3`; the bash and PowerShell
  wrappers fail closed with a config-specific message either way.

A missing python3, a missing `cryptography` package, or an empty key set
remains rc=2 (cannot verify), to keep the "release may be tampered" warning
reserved for actual signature mismatches.

## The embedded public keys

Each consumer holds **up to two** accepted release keys today; the regex that
the release-time verifier (`scripts/verify-signing-key.py`) uses to read the
installers' slot variables matches any third (or further) rotation slot too, so
adding a `PODUP_RELEASE_PUBKEY3_B64` to `install.sh` is a one-line change when
a third key is needed. Slot 0 holds the active key; later slots are the empty
rotation slots, populated only during a rotation. Embedded in three places:

- `internal/update/verify.rs` — `RELEASE_PUBKEYS[0]` and `[1]` (raw 32 bytes).
- `install.sh` — `PODUP_RELEASE_PUBKEY_B64` and `PODUP_RELEASE_PUBKEY2_B64`.
- `install.ps1` — `PubKeyB64` and `PubKey2B64`.

A signature is trusted if it validates under **any** non-empty key. If every key
is zeroed, both the binary and the installer fail closed and install nothing.

### Key rotation

Because each binary accepts up to two keys, the signing key can be rotated
**without stranding installed binaries** — provided the outgoing private key is
still available to sign the migration release. A two-release transition first
ships the new key alongside the old (signed by the old key), so binaries already
in the field accept the next release and gain the new key; a later release then
retires the old key once every install has converged on the new one. The GitHub
build-provenance attestation, which does not depend on the signing key, still
proves origin during the window.

Binaries that predate the current key set cannot verify newer releases in-band;
they are reinstalled once via the current `install.sh` / apt and update normally
from then on.

## Verifying a release independently

Operators who want to verify a release without trusting the installer can do so
with standard tooling. The GitHub build-provenance attestation proves the binary
came from this repository's release workflow:

```bash
gh attestation verify podup-linux-x86_64 --repo Glyndor/podup
```

To verify the Ed25519 signature over `SHA256SUMS` offline (no GitHub CLI), use
the embedded public key:

```bash
python3 - <<'PY'
import base64
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
key = Ed25519PublicKey.from_public_bytes(
    base64.b64decode("HFv7vg5FCY7YyKUDbJhaQSfB9SboJGSblJtFbLmLHzM" + "=="))
key.verify(open("SHA256SUMS.sig", "rb").read(), open("SHA256SUMS", "rb").read())
print("SHA256SUMS signature OK")
PY
sha256sum --check --ignore-missing SHA256SUMS
```

The release also ships a CycloneDX SBOM **per artifact** — one for each binary
(e.g. `podup-linux-x86_64.cdx.json`) and each Debian package
(`podup_<version>_amd64.deb.cdx.json`), listing only that target's actual
dependencies rather than a union across every platform — plus a third-party
license attribution (`NOTICES.html`). Each carries a detached `.sig` that
verifies against the same key.

## Air-gapped installation

Networks that block outbound GitHub have no opt-out from verification — they
carry the artifacts across the boundary instead:

1. On a connected host, download the platform binary, `SHA256SUMS`,
   `SHA256SUMS.sig` (and optionally the SBOM/NOTICES and their `.sig` files).
2. Transfer them to the isolated host on approved media.
3. Verify the Ed25519 signature and checksum there with the offline snippet
   above — the embedded key is the trust anchor, so no network is needed.
4. Install the verified binary manually (`install -m 0755 podup-linux-x86_64
   /usr/local/bin/podup`).

For building from source in an air-gapped environment, vendor the crates on a
connected host (`cargo vendor vendor`), include the `vendor/` tree in the source
tarball, and build the Debian package offline — `debian/rules` automatically
switches Cargo to `--frozen --offline` when `vendor/` is present.
