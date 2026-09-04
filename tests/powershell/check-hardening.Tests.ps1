#Requires -Version 5.1
<#
Drive .github/scripts/check-hardening.ps1 over the four properties, with one
control binary per property. Each control is a copy of the good binary with
exactly one bit cleared in DllCharacteristics, so a check that stopped
looking at one property would show as that control passing.

The good binary is built in-process by writing a minimal PE32+ byte
sequence. The script only reads up to DllCharacteristics (offset 70 of the
optional header), so the rest of the image does not need to be a runnable
PE: the bytes past DllCharacteristics are simply not read.

Shape mirrors tests/fixtures/releases/test.ps1: a function-based harness
with explicit Assert-* helpers and exit codes, rather than Pester, since
this repository's PowerShell fixtures all use the same shape.

Requires: nothing beyond PowerShell 5.1. CI runs this on windows-latest
with `shell: pwsh`, where pwsh is on PATH.
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Script   = Join-Path $RepoRoot '.github\scripts\check-hardening.ps1'

if (-not (Test-Path -LiteralPath $Script)) {
	Write-Host "FAIL: $Script not found" -ForegroundColor Red
	exit 1
}

$pass = 0
$fail = 0

function Assert-Eq {
	param([string]$Desc, [string]$Expected, [string]$Actual)
	if ($Expected -eq $Actual) {
		Write-Host "ok    $Desc"
		$script:pass++
	} else {
		Write-Host "FAIL  $Desc"
		Write-Host ("        expected: {0}" -f $Expected)
		Write-Host ("        actual:   {0}" -f $Actual)
		$script:fail++
	}
}

# PE32+ byte sequence the parser will accept. Offsets below match
# .github/scripts/check-hardening.ps1's reads.
#
#   0..63     DOS header, e_lfanew at 0x3C = 64
#   64..67    PE signature ("PE\0\0")
#   68..87    COFF header
#   88..      Optional header, Magic at 88, DllCharacteristics at 158
$GoodDllChars = 0x0020 -bor 0x0040 -bor 0x0100 -bor 0x4000
$Size         = 164

function New-GoodPe {
	$buf = New-Object byte[] $Size
	$buf[0]  = 0x4D  # 'M'
	$buf[1]  = 0x5A  # 'Z'
	# e_lfanew at 0x3C = 64
	[BitConverter]::GetBytes([uint32]64).CopyTo($buf, 0x3C)
	# PE signature at 64
	$buf[64] = 0x50; $buf[65] = 0x45; $buf[66] = 0; $buf[67] = 0
	# COFF header at 68
	[BitConverter]::GetBytes([uint16]0x8664).CopyTo($buf, 68)  # Machine: AMD64
	[BitConverter]::GetBytes([uint16]112).CopyTo($buf, 84)     # SizeOfOptionalHeader
	[BitConverter]::GetBytes([uint16]0x2102).CopyTo($buf, 86)  # Characteristics
	# Optional header at 88
	[BitConverter]::GetBytes([uint16]0x20b).CopyTo($buf, 88)   # Magic: PE32+
	# DllCharacteristics at 88 + 70 = 158
	[BitConverter]::GetBytes([uint16]$GoodDllChars).CopyTo($buf, 158)
	return ,$buf
}

function Write-PeWith([byte[]]$Source, [string]$Path, [uint16]$ClearBit) {
	$copy = New-Object byte[] $Size
	[Array]::Copy($Source, $copy, $Size)
	$current = [BitConverter]::ToUInt16($copy, 158)
	$next = $current -band (-bnot $ClearBit)
	[BitConverter]::GetBytes([uint16]$next).CopyTo($copy, 158)
	[System.IO.File]::WriteAllBytes($Path, $copy)
}

# Properties the script reports, paired with the bit each clears. The
# name is what check-hardening.ps1 prints on FAIL.
$PROPS = @(
	@{ Name = 'dynamic-base';     Bit = [uint16]0x0040 },
	@{ Name = 'high-entropy-va';  Bit = [uint16]0x0020 },
	@{ Name = 'nx-compat';        Bit = [uint16]0x0100 },
	@{ Name = 'guard-cf';         Bit = [uint16]0x4000 }
)

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
[System.IO.Directory]::CreateDirectory($tmp) | Out-Null
try {
	$good = New-GoodPe
	$goodPath = Join-Path $tmp 'good'
	[System.IO.File]::WriteAllBytes($goodPath, $good)
	$notPePath = Join-Path $tmp 'not-pe'
	[System.IO.File]::WriteAllBytes($notPePath, [System.Text.Encoding]::ASCII.GetBytes("this is not a pe file`n"))

	# 1. Good binary on its own passes.
	$stdout = & pwsh -NoProfile -File $Script $goodPath 2>&1
	$rc = $LASTEXITCODE
	Assert-Eq 'a PE with all four properties passes' '0' "$rc"
	Assert-Eq 'and is reported as ok' "ok    $goodPath" ($stdout -join "`n")

	# 2. One control per property, each fails for its own name.
	foreach ($p in $PROPS) {
		$ctl = Join-Path $tmp ("missing-" + $p.Name)
		Write-PeWith -Source $good -Path $ctl -ClearBit $p.Bit
		$stdout = & pwsh -NoProfile -File $Script $ctl 2>&1
		$rc = $LASTEXITCODE
		Assert-Eq ("$($p.Name) control fails") '1' "$rc"
		Assert-Eq ("$($p.Name) is failed for its own name") "FAIL $($p.Name)  $ctl" ($stdout -join "`n")
	}

	# 3. Non-PE file fails as 'not-pe'.
	$stdout = & pwsh -NoProfile -File $Script $notPePath 2>&1
	$rc = $LASTEXITCODE
	Assert-Eq 'a non-PE file fails' '1' "$rc"
	Assert-Eq 'and says it is not a PE' "FAIL not-pe  $notPePath" ($stdout -join "`n")

	# 4. A bad file among good ones fails the run, and the good ones still
	#    report ok.
	$bad = Join-Path $tmp 'missing-guard-cf'
	$stdout = & pwsh -NoProfile -File $Script $goodPath $bad $goodPath 2>&1
	$rc = $LASTEXITCODE
	Assert-Eq 'one bad file among good ones fails the run' '1' "$rc"
	Assert-Eq 'the good ones are still listed as ok' '2' ("$(($stdout -join "`n") -split "`n" | Where-Object { $_ -like 'ok    *' }).Count")

	# 5. No arguments is a usage error. Spawn pwsh so $LASTEXITCODE reflects
	#    the script's own exit code (running a script via `&` in the same
	#    scope does not propagate exit to $LASTEXITCODE).
	$out = & pwsh -NoProfile -File $Script 2>&1
	$rc = $LASTEXITCODE
	Assert-Eq 'no arguments is a usage error, exit 2' '2' "$rc"
} finally {
	Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host "passed: $pass  failed: $fail"
exit ($fail -eq 0 ? 0 : 1)
