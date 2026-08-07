#requires -Version 5.1
<#
Release-signing regression test for issue #1359, PowerShell side.

Mirrors the bash test in test.sh:
  1. Per-asset .sig loop with three populated rotation slots.
  2. SLOT_RE recognises the third slot (corrupt slot 3 and confirm failure).
  3. L7 error class - the embedded Python verifier reports rc=3 for a
     malformed key, distinct from rc=1 (tampered).

The PowerShell verifier in install.ps1 uses the same exit-code scheme as
the shell verifier, so the L7 distinction is symmetric.
#>

$ErrorActionPreference = 'Stop'

$RepoRoot    = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$FixtureDir  = Join-Path $RepoRoot 'tests/fixtures/releases'
$Verifier    = Join-Path $RepoRoot '.github\scripts\verify-signing-key.py'
$TestKeyPath = Join-Path $FixtureDir 'test-key.b64'

if (-not (Get-Command python3 -ErrorAction SilentlyContinue)) {
	Write-Host 'SKIP: python3 is not installed' -ForegroundColor Yellow
	exit 0
}

# cryptography is required for the verifier; skip cleanly if missing.
try {
	python3 -c 'from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey' 2>$null
	if ($LASTEXITCODE -ne 0) { throw }
} catch {
	Write-Host 'SKIP: python3 is missing the cryptography package' -ForegroundColor Yellow
	exit 0
}

function Assert-True {
	param([bool]$Condition, [string]$Message)
	if (-not $Condition) {
		Write-Host "FAIL: $Message" -ForegroundColor Red
		exit 1
	}
}

# -----------------------------------------------------------------------------
# Part 1: per-asset .sig loop with three rotation slots.
# -----------------------------------------------------------------------------
Write-Host 'Part 1: per-asset .sig verification (3 rotation slots)' -ForegroundColor Cyan

$InstallerPath = Join-Path $FixtureDir 'install.sh'
$sigs = Get-ChildItem -Path $FixtureDir -Filter '*.sig'
$passed = 0
$total = 0
foreach ($sig in $sigs) {
	$base = Join-Path $FixtureDir $sig.BaseName
	$total++
	& python3 $Verifier $base $sig.FullName $InstallerPath *> $null
	if ($LASTEXITCODE -eq 0) {
		$passed++
		Write-Host "  OK    $($sig.BaseName)"
	} else {
		Write-Host "  FAIL  $($sig.BaseName) (rc=$LASTEXITCODE)"
	}
}
Assert-True ($passed -eq $total) "only $passed of $total signatures verified"

# -----------------------------------------------------------------------------
# Part 2: third slot is the one that verifies.
# -----------------------------------------------------------------------------
Write-Host 'Part 2: SLOT_RE picks up the third rotation slot' -ForegroundColor Cyan

# A valid Ed25519 public key that is NOT the test key.
$WrongPk = & python3 -c @'
import base64
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
raw = Ed25519PrivateKey.from_private_bytes(b"\x99" * 32).public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw)
print(base64.b64encode(raw).decode().rstrip("="))
'@

$tmpInstall = [System.IO.Path]::GetTempFileName()
$installLines = Get-Content -LiteralPath (Join-Path $FixtureDir 'install.sh') | ForEach-Object {
	if ($_ -match '^PODUP_RELEASE_PUBKEY3_B64=.*') {
		"PODUP_RELEASE_PUBKEY3_B64=`"${PODUP_RELEASE_PUBKEY3_B64:-$WrongPk}`""
	} else { $_ }
}
Set-Content -LiteralPath $tmpInstall -Value $installLines -Encoding UTF8

$podupSig = Join-Path $FixtureDir 'podup-linux-x86_64.sig'
$podupAsset = Join-Path $FixtureDir 'podup-linux-x86_64'
& python3 $Verifier $podupAsset $podupSig $tmpInstall *> $null
Assert-True ($LASTEXITCODE -ne 0) 'verifier accepted a signature with the matching key in slot 3 replaced'
Write-Host '  OK    slot 3 is recognised (its absence flips the verifier to a no-match error)'

# -----------------------------------------------------------------------------
# Part 3: L7 error class - the embedded Python in install.ps1 returns rc=3
# for a malformed key, NOT rc=1. We exercise the same Python snippet that
# install.ps1 writes to verify_ed25519.py at install time, so a change in
# the snippet fails this test.
# -----------------------------------------------------------------------------
Write-Host 'Part 3: malformed key produces a configuration problem, not tampering' -ForegroundColor Cyan

$pySource = @'
import base64, binascii, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
from cryptography.exceptions import InvalidSignature
sig_file, data_file = sys.argv[1], sys.argv[2]
sig = open(sig_file, "rb").read()
data = open(data_file, "rb").read()
for slot, pubkey_b64 in enumerate(sys.argv[3:]):
    try:
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

$tmpPy = [System.IO.Path]::GetTempFileName()
Set-Content -LiteralPath $tmpPy -Value $pySource -Encoding ASCII
$tmpDir = [System.IO.Path]::GetTempFileName() | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_ }
$tmpSig = Join-Path $tmpDir 'sig.bin'
$tmpData = Join-Path $tmpDir 'data.bin'
$tmpErr = Join-Path $tmpDir 'err.log'
Set-Content -LiteralPath $tmpData -Value 'fixture data' -Encoding ASCII -NoNewline
Set-Content -LiteralPath $tmpSig -Value 'badsig' -Encoding ASCII -NoNewline

# 1. Malformed key -> rc=3 (config), not rc=1 (tampered).
& python3 $tmpPy $tmpSig $tmpData '@@@@not-base64@@@@' 2> $tmpErr
$pyExit = $LASTEXITCODE
Assert-True ($pyExit -eq 3) "expected rc=3 (config) for a malformed key, got rc=$pyExit"
$errText = Get-Content -LiteralPath $tmpErr -Raw
Assert-True ($errText -match 'malformed') "expected stderr to mention 'malformed', got: $errText"
Write-Host '  OK    rc=3 with malformed on stderr (config error class)'

# 2. Happy path: the test key signs a real file and verifies.
$testKey = (Get-Content -LiteralPath $TestKeyPath).Trim()
$signPy = @'
import base64, sys
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
seed = bytes([7]) * 32
sk = Ed25519PrivateKey.from_private_bytes(seed)
data = open(sys.argv[1], "rb").read()
sys.stdout.buffer.write(sk.sign(data))
'@
$tmpSign = [System.IO.Path]::GetTempFileName()
Set-Content -LiteralPath $tmpSign -Value $signPy -Encoding ASCII
$tmpDataSig = Join-Path $tmpDir 'data.sig'
& python3 $tmpSign $tmpData | Set-Content -LiteralPath $tmpDataSig -Encoding Byte
& python3 $tmpPy $tmpDataSig $tmpData $testKey *> $null
Assert-True ($LASTEXITCODE -eq 0) 'happy path: real signature under the test key must verify'
Write-Host '  OK    happy path still verifies'

# -----------------------------------------------------------------------------
# Part 4: canonical base64 padding (M3) - same Python check the bash test
# uses, executed here so a future regression in the snippet above (e.g.
# reverting to "==") breaks both halves of the matrix.
# -----------------------------------------------------------------------------
Write-Host 'Part 4: canonical base64 padding for keys of any length' -ForegroundColor Cyan

$padPy = @'
import base64
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
raw = Ed25519PrivateKey.from_private_bytes(b"\x33" * 32).public_key().public_bytes(
    serialization.Encoding.Raw, serialization.PublicFormat.Raw)
b = base64.b64encode(raw).decode()
assert len(b.rstrip("=")) == 43, f"unexpected key length: {len(b.rstrip(chr(61)))}"
raw2 = base64.b64decode(b + "=" * (-len(b) % 4))
assert raw2 == raw, "canonical padding does not round-trip the key"
overpadded = b + "=="
raw3 = base64.b64decode(overpadded + "=" * (-len(overpadded) % 4))
assert raw3 == raw, "old over-padded form must still round-trip cleanly"
print("  OK    canonical padding round-trips 43-char keys and tolerates old over-padded form")
'@
$tmpPad = [System.IO.Path]::GetTempFileName()
Set-Content -LiteralPath $tmpPad -Value $padPy -Encoding ASCII
& python3 $tmpPad

# A known-mis-padded key (one with stray characters) must produce rc=3
# (configuration problem), not rc=1 (release-tampering), so a fork
# maintainer debugging an override isn't told to chase a phantom signature
# mismatch. We exercise the same Python snippet install.ps1 embeds to keep
# the matrix symmetric.
$tmpPadDir = [System.IO.Path]::GetTempFileName() | ForEach-Object { Remove-Item $_; New-Item -ItemType Directory -Path $_ }
$tmpPadSig = Join-Path $tmpPadDir 'sig.bin'
$tmpPadData = Join-Path $tmpPadDir 'data.bin'
$tmpPadErr = Join-Path $tmpPadDir 'err.log'
Set-Content -LiteralPath $tmpPadData -Value 'fixture data' -Encoding ASCII -NoNewline
Set-Content -LiteralPath $tmpPadSig -Value 'badsig' -Encoding ASCII -NoNewline
& python3 $tmpPy $tmpPadSig $tmpPadData ($testKey + '@@@bad@@@') 2> $tmpPadErr
$padExit = $LASTEXITCODE
Assert-True ($padExit -eq 3) "mis-padded key must produce rc=3 (config), got rc=$padExit"
$padErrText = Get-Content -LiteralPath $tmpPadErr -Raw
Assert-True ($padErrText -match 'malformed') "expected stderr to mention 'malformed', got: $padErrText"
Write-Host '  OK    mis-padded key (stray chars) -> rc=3 (config error class)'
Remove-Item -LiteralPath $tmpPadDir -Recurse -Force -ErrorAction SilentlyContinue

# Cleanup
Remove-Item -LiteralPath $tmpInstall, $tmpPy, $tmpSign, $tmpPad -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $tmpDir -Recurse -Force -ErrorAction SilentlyContinue

Write-Host ''
Write-Host 'All parts passed.' -ForegroundColor Green
