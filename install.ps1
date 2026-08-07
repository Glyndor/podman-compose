#Requires -Version 5.1
#
# podup installer for Windows - downloads a release binary, verifies it and
# installs it.
#
# Usage:
#   irm https://glyndor.net/podup/install/windows | iex
#
# Environment:
#   PODUP_VERSION              Release tag to install (e.g. v0.3.0). Default: latest.
#   PODUP_INSTALL_DIR          Installation directory. Default: %LOCALAPPDATA%\Programs\podup.
#   PODUP_RELEASE_PUBKEY_B64   Override the baked-in Ed25519 release public key (for forks).

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
# PowerShell 7.3+ turns a non-zero native exit into a terminating error under
# ErrorActionPreference='Stop'. We branch on $LASTEXITCODE ourselves (a failed
# signature check is expected control flow, not a fatal error), so opt out.
# Harmless no-op on Windows PowerShell 5.1, which lacks this variable.
$PSNativeCommandUseErrorActionPreference = $false

$Repo = 'Glyndor/podup'
$Version = if ($env:PODUP_VERSION) { $env:PODUP_VERSION } else { 'latest' }
$InstallDir = if ($env:PODUP_INSTALL_DIR) { $env:PODUP_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\podup' }

function Write-LogInfo($msg)  { Write-Host "[info] $msg" -ForegroundColor Blue }
function Write-LogOk($msg)    { Write-Host "[ ok ] $msg" -ForegroundColor Green }
function Write-LogError($msg) { Write-Host "[fail] $msg" -ForegroundColor Red }
function Fail($msg) { Write-LogError $msg; exit 1 }

# --- Platform detection ------------------------------------------------------

$osArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
switch ($osArch) {
	'X64'   { $Arch = 'x86_64' }
	'Arm64' { $Arch = 'arm64' }
	default { Fail "Unsupported architecture: $osArch (supported: x86_64, arm64)" }
}

$Artifact = "podup-windows-$Arch.exe"

# --- Resolve download URL ----------------------------------------------------

if ($Version -eq 'latest') {
	$BaseUrl = "https://github.com/$Repo/releases/latest/download"
} elseif ($Version -match '^v[0-9]+\.[0-9]+\.[0-9]+$') {
	$BaseUrl = "https://github.com/$Repo/releases/download/$Version"
} else {
	Fail "PODUP_VERSION must be 'latest' or a semver tag like v1.2.3, got: $Version"
}

# Windows PowerShell 5.1 defaults to TLS 1.0/1.1; force at least TLS 1.2 for
# GitHub, and allow TLS 1.3 too where the host's .NET Framework defines it
# (an exact Tls12 assignment would exclude a newer, already-supported
# protocol; older .NET Framework builds do not expose the Tls13 member, so
# fall back to Tls12 alone rather than fail the install over it).
try {
	[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
} catch {
	[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}

$TmpDir = New-Item -ItemType Directory -Path (Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName()))

try {
	# --- Download ------------------------------------------------------------

	# 200 MB ceiling on any downloaded release asset. Invoke-WebRequest runs the
	# download to completion before -OutFile can be checked, so this is a
	# post-hoc guard - it rejects an oversized file after it lands on disk, it
	# does not cap a hostile stream mid-flight. -TimeoutSec is the bound that
	# actually applies while the request is in flight: it caps the whole
	# request so a stalled or never-ending response cannot hang the installer.
	$MaxDownloadBytes = 209715200
	$DownloadTimeoutSec = 300

	function Get-ReleaseFile($name) {
		$dest = Join-Path $TmpDir $name
		$url = "$BaseUrl/$name"
		try {
			Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing -TimeoutSec $DownloadTimeoutSec
		} catch {
			Fail "Download failed: $url"
		}
		if ((Get-Item -Path $dest).Length -gt $MaxDownloadBytes) {
			Fail "Download too large (over 200 MB): $url"
		}
		return $dest
	}

	Write-LogInfo "Downloading $Artifact ($Version) ..."
	$artifactPath = Get-ReleaseFile $Artifact
	$sumsPath = Get-ReleaseFile 'SHA256SUMS'
	$sigPath  = Get-ReleaseFile 'SHA256SUMS.sig'

	# --- Verify --------------------------------------------------------------

	# Checksum alone is not a trust anchor: a tampered release can ship a matching
	# SHA256SUMS. The binary is trusted only after at least one cryptographic proof
	# tied to the release key or the repository's build identity succeeds - the
	# Ed25519 signature over SHA256SUMS, or the GitHub build-provenance attestation.
	# If neither verifier can run, the install fails closed.

	# Baked-in base64 (unpadded) raw Ed25519 public keys (32 bytes each) matching
	# the release signing key. Up to two are accepted: the
	# second is empty except during a key rotation, when it holds the new key so a
	# release signed by either key verifies. The signature passes if any key
	# validates. Override for a fork via PODUP_RELEASE_PUBKEY_B64 / _PUBKEY2_B64.
	$PubKeyB64  = if ($env:PODUP_RELEASE_PUBKEY_B64) { $env:PODUP_RELEASE_PUBKEY_B64 } else { 'HFv7vg5FCY7YyKUDbJhaQSfB9SboJGSblJtFbLmLHzM' }
	$PubKey2B64 = if ($env:PODUP_RELEASE_PUBKEY2_B64) { $env:PODUP_RELEASE_PUBKEY2_B64 } else { '' }
	$PubKeys = @($PubKeyB64, $PubKey2B64 | Where-Object { $_ })

	$verified = $false

	# Locate a python interpreter that has the 'cryptography' package. Each
	# candidate carries any leading args (the 'py' launcher needs '-3').
	function Find-Python {
		$candidates = @(
			@{ Exe = 'python3'; Pre = @() },
			@{ Exe = 'python';  Pre = @() },
			@{ Exe = 'py';      Pre = @('-3') }
		)
		foreach ($c in $candidates) {
			if (-not (Get-Command $c.Exe -ErrorAction SilentlyContinue)) { continue }
			$probeArgs = $c.Pre + @('-c', 'from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey')
			& $c.Exe @probeArgs 2>$null
			if ($LASTEXITCODE -eq 0) { return $c }
		}
		return $null
	}

	Write-LogInfo 'Verifying SHA256SUMS signature ...'
	if ($PubKeys.Count -gt 0) {
		$python = Find-Python
		if ($python) {
			$pyScript = Join-Path $TmpDir 'verify_ed25519.py'
			# Python source - indentation is significant, keep as-is.
			# Exit codes: 0 verified, 1 signature present but INVALID (tampered),
			# 2 unused on this path (no python3/cryptography already handled
			# above), 3 the configured release key is malformed (a configuration
			# problem, kept distinct from rc=1 so the caller can report it
			# without scaring the user about the release).
			$pySource = @'
import base64, binascii, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from cryptography.exceptions import InvalidSignature
sig_file, data_file = sys.argv[1], sys.argv[2]
sig = open(sig_file, "rb").read()
data = open(data_file, "rb").read()
for slot, pubkey_b64 in enumerate(sys.argv[3:]):
    try:
        # Pad to a 4-byte boundary the way sign.py does: the installer stores
        # the key unpadded, and a stricter decoder would reject a fixed two-"="
        # suffix when the key is already a multiple of four chars long.
        raw = base64.b64decode(pubkey_b64 + "=" * (-len(pubkey_b64) % 4))
        Ed25519PublicKey.from_public_bytes(raw).verify(sig, data)
        sys.exit(0)
    except (binascii.Error, ValueError) as exc:
        print("configured release key slot %d is malformed: %s" % (slot, exc), file=sys.stderr)
        sys.exit(3)
    except InvalidSignature:
        continue
sys.exit(1)
'@
			Set-Content -Path $pyScript -Value $pySource -Encoding ASCII
			$pyArgs = $python.Pre + @($pyScript, $sigPath, $sumsPath) + $PubKeys
			& $python.Exe @pyArgs
			$pyExit = $LASTEXITCODE
			if ($pyExit -eq 0) {
				Write-LogOk 'SHA256SUMS signature verified'
				$verified = $true
			} elseif ($pyExit -eq 3) {
				Fail 'Configured release key is malformed - check PODUP_RELEASE_PUBKEY_B64 / PODUP_RELEASE_PUBKEY2_B64 environment variables and re-run'
			} else {
				Fail 'SHA256SUMS signature verification failed - release may be tampered'
			}
		} else {
			# A release public key IS configured: the pinned key is the trust anchor
			# and must not be silently bypassed in favour of the (repo-scoped)
			# attestation. Fail closed.
			Fail "python3 with the 'cryptography' package is required to verify the release signature against the pinned key. Install it and re-run."
		}
	} else {
		Write-LogInfo 'no release public key configured - skipping Ed25519 signature check'
	}

	# Build-provenance attestation: proves the binary was produced by this repo's
	# release workflow (GitHub OIDC). Defence-in-depth next to the pinned key; the
	# trust anchor when no release public key is configured. Pinned to the release
	# workflow - a repo-scoped check would accept an attestation from any workflow
	# in the repo.
	$ghAttestation = $false
	if (Get-Command gh -ErrorAction SilentlyContinue) {
		& gh attestation --help *> $null
		if ($LASTEXITCODE -eq 0) { $ghAttestation = $true }
	}
	if ($ghAttestation) {
		Write-LogInfo 'Verifying artifact attestation ...'
		& gh attestation verify $artifactPath --repo $Repo --signer-workflow "$Repo/.github/workflows/release.yml" | Out-Null
		if ($LASTEXITCODE -ne 0) { Fail "Attestation verification failed for $Artifact" }
		Write-LogOk 'Attestation verified'
		$verified = $true
	} else {
		Write-LogInfo 'GitHub CLI with attestation support not found - cannot check attestation'
	}

	# Fail closed: a strong cryptographic proof is mandatory. A checksum alone is not
	# a trust anchor, and there is no opt-out - hardened environments require
	# verifiable supply-chain integrity at install time.
	if (-not $verified) {
		Fail "No signature or attestation verifier available. Install 'gh' (>= 2.49) or python3 with the 'cryptography' package, or set PODUP_RELEASE_PUBKEY_B64, then re-run."
	}

	Write-LogInfo 'Verifying SHA-256 checksum ...'
	$expectedLine = Select-String -Path $sumsPath -Pattern ("\s" + [regex]::Escape($Artifact) + "$") | Select-Object -First 1
	if (-not $expectedLine) { Fail "No checksum entry for $Artifact in SHA256SUMS" }
	$expected = ($expectedLine.Line -split '\s+')[0].ToLower()
	$actual = (Get-FileHash -Path $artifactPath -Algorithm SHA256).Hash.ToLower()
	if ($expected -ne $actual) { Fail "Checksum verification failed for $Artifact" }
	Write-LogOk 'Checksum verified'

	# --- Install -------------------------------------------------------------

	if (-not (Test-Path $InstallDir)) {
		New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
	}
	$target = Join-Path $InstallDir 'podup.exe'
	# Stage next to the target and rename into place, so an interrupted install
	# can never leave a partial yet executable binary on PATH.
	$staged = Join-Path $InstallDir ".podup.install-$PID.exe"
	Copy-Item -Path $artifactPath -Destination $staged -Force
	Move-Item -Path $staged -Destination $target -Force

	# Add the install dir to the user PATH if it is not already there.
	$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
	$onPath = ($userPath -split ';') -contains $InstallDir
	if (-not $onPath) {
		$newPath = if ([string]::IsNullOrEmpty($userPath)) { $InstallDir } else { "$userPath;$InstallDir" }
		[Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
		$env:Path = "$env:Path;$InstallDir"
		Write-LogInfo "Added $InstallDir to your user PATH (restart your shell to pick it up)"
	}

	$installed = & $target --version
	Write-LogOk "podup installed: $installed"
} finally {
	Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
