#Requires -Version 5.1
<#
Assert the hardening the Windows release binaries are documented with, over
every PE file named on the command line. Four properties, each read off the
PE optional header's DllCharacteristics:

  DYNAMIC_BASE     ASLR: the loader can relocate the image
  HIGH_ENTROPY_VA  64-bit ASLR with the full entropy range
  NX_COMPAT        DEP: the image is compatible with a non-executable stack
  GUARD_CF         Control Flow Guard: the linker emits CFG check tables

The header is parsed in-process from the file bytes; no Visual Studio
tooling (dumpbin, link /dump) is required and no native executable is
invoked, so the script works on every Windows runner without setup.

PE structure (PE32+, what both x86_64 and aarch64 Windows binaries are):

   offset    size   field
        0      2    "MZ" (e_magic)
       60      4    e_lfanew          (little-endian, points at PE signature)
  e_lfanew     4    "PE\0\0"          (PE signature)
  +4           2    Machine           (COFF header start)
  +6           2    NumberOfSections
  +8           4    TimeDateStamp
  +12          4    PointerToSymbolTable
  +16          4    NumberOfSymbols
  +20          2    SizeOfOptionalHeader
  +22          2    Characteristics
  +24          2    Magic             (optional header start; 0x20b = PE32+)
  +94          2    DllCharacteristics  (offset 70 from optional header)

DllCharacteristics bits:
  0x0020  IMAGE_DLLCHARACTERISTICS_HIGH_ENTROPY_VA
  0x0040  IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE
  0x0100  IMAGE_DLLCHARACTERISTICS_NX_COMPAT
  0x4000  IMAGE_DLLCHARACTERISTICS_GUARD_CF

Output: one line per file, "ok" or "FAIL <property> <file>", with a non-zero
exit when any file fails any property. Exit 2 with a usage line when no
argument is given.
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ($args.Count -eq 0) {
	[Console]::Error.WriteLine("usage: $PSCommandPath <pe>...")
	exit 2
}

# Bit names, in the order the script reports them. The order matches the
# order in the Linux check-hardening.sh so the four scripts read alike: a
# reviewer comparing the two gates is not also re-learning the property
# order.
$PROPERTIES = @(
	@{ Name = 'dynamic-base';  Bit = 0x0040 },
	@{ Name = 'high-entropy-va'; Bit = 0x0020 },
	@{ Name = 'nx-compat';     Bit = 0x0100 },
	@{ Name = 'guard-cf';      Bit = 0x4000 }
)

$DOS_E_LFANEW = 0x3C
$DOS_MAGIC = 0x5A4D  # 'MZ' as a little-endian uint16
$PE_SIG = 0x00004550  # "PE\0\0" as a little-endian uint32
$OPT_HDR_MAGIC_PE32_PLUS = 0x20b
$OPT_HDR_MAGIC_PE32      = 0x10b
$DLLCHARACTERISTICS_OFFSET = 70  # within the optional header

# Each byte is widened to [int] before the shift. A [byte] shifted left in
# PowerShell stays a byte and drops the high bits, which read "MZ" as 77.
function Read-UInt16LE([byte[]]$Buf, [int]$Off) {
	return [uint16]([int]$Buf[$Off] -bor ([int]$Buf[$Off + 1] -shl 8))
}

function Read-UInt32LE([byte[]]$Buf, [int]$Off) {
	return [uint32](
		([int]$Buf[$Off] -bor ([int]$Buf[$Off + 1] -shl 8) -bor ([int]$Buf[$Off + 2] -shl 16) -bor ([int]$Buf[$Off + 3] -shl 24))
	)
}

$status = 0
foreach ($path in $args) {
	$failed = @()

	# A non-existent file, or a file too small to be a PE, is "not-pe". The
	# minimum byte length for the script to read up to DllCharacteristics
	# end is e_lfanew (>=64) + 4 (signature) + 20 (COFF) + 72 (optional up
	# to DllCharacteristics end) = 160 bytes. We only require the bytes
	# up to that end be present; the script never reads past it.
	if (-not (Test-Path -LiteralPath $path)) {
		Write-Output "FAIL not-pe  $path"
		$status = 1
		continue
	}

	$bytes = [System.IO.File]::ReadAllBytes($path)
	if ($bytes.Length -lt ($DOS_E_LFANEW + 4 + 4)) {
		Write-Output "FAIL not-pe  $path"
		$status = 1
		continue
	}

	if ((Read-UInt16LE $bytes 0) -ne $DOS_MAGIC) {
		Write-Output "FAIL not-pe  $path"
		$status = 1
		continue
	}

	$peSigOff = [int](Read-UInt32LE $bytes $DOS_E_LFANEW)
	# The optional header begins 24 bytes into the COFF header; reading
	# past DllCharacteristics needs another 72 bytes past the magic.
	$needLen = $peSigOff + 4 + 24 + ($DLLCHARACTERISTICS_OFFSET + 2)
	if ($peSigOff -lt 0 -or $needLen -gt $bytes.Length) {
		Write-Output "FAIL not-pe  $path"
		$status = 1
		continue
	}

	if ((Read-UInt32LE $bytes $peSigOff) -ne $PE_SIG) {
		Write-Output "FAIL not-pe  $path"
		$status = 1
		continue
	}

	$optHdrOff = $peSigOff + 4 + 20  # skip signature + COFF header
	$magic = Read-UInt16LE $bytes $optHdrOff
	if ($magic -ne $OPT_HDR_MAGIC_PE32_PLUS -and $magic -ne $OPT_HDR_MAGIC_PE32) {
		Write-Output "FAIL not-pe  $path"
		$status = 1
		continue
	}

	$dllCharsOff = $optHdrOff + $DLLCHARACTERISTICS_OFFSET
	$dllChars = Read-UInt16LE $bytes $dllCharsOff

	foreach ($p in $PROPERTIES) {
		if (($dllChars -band $p.Bit) -eq 0) {
			$failed += $p.Name
		}
	}

	if ($failed.Count -eq 0) {
		Write-Output "ok    $path"
	} else {
		Write-Output "FAIL $($failed -join ' ')  $path"
		$status = 1
	}
}

exit $status
