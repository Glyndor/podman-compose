#requires -Version 5.1
<#
Version self-test regression for issue #1356, PowerShell side.

Mirrors the bash test in version-self-test.sh:
  1. A stub that reports the resolved tag with v prefix passes the
     self-test.
  2. A stub that reports the resolved tag without v prefix also passes.
  3. A stub that reports an older tag is rejected.
  4. A stub that reports a -dev suffix is rejected (the rollback case:
     a partial token would let `3.7.0-dev` slip past `3.7.0`).
  5. A stub that reports garbage is rejected.
  6. A stub that exits non-zero on --version is rejected.
  7. A staged file that does not exist is rejected.
  8. On any failed self-test the staged file is removed.

The actual Test-StagedVersion function lives in install.ps1; this
script extracts it via AST and loads it in this scope so the regression
runs against the real code, not a copy. (The bash test uses
`sed + source`, which PowerShell cannot mirror directly; AST parsing
is the equivalent and stays robust to surrounding code.)

Test-StagedVersion calls the top-level `Fail` helper on a mismatch.
`Fail` does `exit 1` and that would terminate the whole test process.
`Invoke-Expression` binds the extracted function to the test's scope,
so its reference to `Fail` resolves to a test-local override that
throws - the test catches the throw and reports a clean refusal.
#>

$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..\..')).Path
$InstallerPath = Join-Path $RepoRoot 'install.ps1'

# Override the helpers Test-StagedVersion calls: route the refusal to a
# throw instead of `exit 1`, and silence the side-effect log lines so
# the test output stays readable. The overrides shadow install.ps1's
# top-level definitions; Invoke-Expression binds the extracted
# function to this scope, so its references resolve here.
function Write-LogInfo { param($msg) Write-Host "[info] $msg" -ForegroundColor Blue }
function Write-LogOk { param($msg) Write-Host "[ ok ] $msg" -ForegroundColor Green }
function Write-LogError { param($msg) Write-Host "[fail] $msg" -ForegroundColor Red }
function Fail { param($msg) throw [System.Exception]::new("Refused: $msg") }

# Extract Test-StagedVersion via AST so we test the real code, not a
# copy. Fail closed if the function is missing: a regression that
# removes the gate from install.ps1 must break this test.
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
	$InstallerPath, [ref]$null, [ref]$null)
$testFn = $ast.Find({ param($n)
	$n -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
	$n.Name -eq 'Test-StagedVersion'
}, $true)
if (-not $testFn) {
	Write-Host 'FAIL: Test-StagedVersion not found in install.ps1' -ForegroundColor Red
	exit 1
}
Invoke-Expression $testFn.Extent.Text

function Assert-True {
	param([bool] $Condition, [string] $Message)
	if (-not $Condition) {
		Write-Host "FAIL: $Message" -ForegroundColor Red
		exit 1
	}
}

# Stub directory: write platform-appropriate executable stubs that
# print specific --version outputs. On Windows we use .cmd files
# (natively executable). On Linux/macOS we use bash scripts with a
# shebang, since .cmd files have no kernel handler there. The test
# fixture's logical contract is the same in both cases - what matters
# is what `& <stub> --version` prints and the exit code it returns.
$stubsDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $stubsDir | Out-Null
try {
	$script:useWindowsStubs = [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform(
		[System.Runtime.InteropServices.OSPlatform]::Windows)
	$stubExt = if ($script:useWindowsStubs) { '.cmd' } else { '.sh' }
	# The .sh stub template runs the body and exits with the requested
	# code. The .cmd stub uses cmd.exe semantics.
	function New-Stub {
		param([string] $Name, [string] $Body, [string] $ExitCode = '0')
		$path = Join-Path $stubsDir ("$Name$stubExt")
		if ($script:useWindowsStubs) {
			$content = "@echo off`r`n$Body`r`nexit /b $ExitCode"
			Set-Content -LiteralPath $path -Value $content -Encoding ASCII -NoNewline
		} else {
			$content = "#!/bin/sh`n$Body`nexit $ExitCode"
			Set-Content -LiteralPath $path -Value $content -Encoding ASCII
			& chmod +x $path
		}
		return $path
	}

	function Invoke-Pass {
		param([string] $StubPath, [string] $Tag)
		try {
			Test-StagedVersion -StagedPath $StubPath -ResolvedTag $Tag
		} catch {
			# Test-StagedVersion's success path is silent apart from the
			# info line routed through Write-LogInfo (now silenced). A
			# throw here means the test override threw via Fail - that
			# is the refusal case, which Invoke-Fail handles.
			Write-Host "  FAIL  $StubPath should have passed but was refused: $_" -ForegroundColor Red
			exit 1
		}
		Write-Host "  OK    $StubPath reports the resolved tag"
	}

	function Invoke-Fail {
		param([string] $StubPath, [string] $Tag)
		$refused = $false
		try {
			Test-StagedVersion -StagedPath $StubPath -ResolvedTag $Tag
		} catch {
			$refused = $true
		}
		Assert-True $refused "$StubPath should have been refused but the self-test accepted it"
		Assert-True (-not (Test-Path -LiteralPath $StubPath)) "staged file $StubPath was not removed after the failed self-test"
		Write-Host "  OK    $StubPath was refused and the staged file was removed"
	}

	$Tag = 'v3.7.0'

	Write-Host 'Part 1: --version reports the resolved tag (with v prefix)' -ForegroundColor Cyan
	$stub = New-Stub -Name 'pass_v' -Body 'echo podup version v3.7.0'
	Invoke-Pass -StubPath $stub -Tag $Tag

	Write-Host 'Part 2: --version reports the resolved tag (without v prefix)' -ForegroundColor Cyan
	$stub = New-Stub -Name 'pass_plain' -Body 'echo podup 3.7.0'
	Invoke-Pass -StubPath $stub -Tag $Tag

	Write-Host 'Part 3: --version reports an older tag (rollback)' -ForegroundColor Cyan
	$stub = New-Stub -Name 'fail_older' -Body 'echo podup version v3.6.0'
	Invoke-Fail -StubPath $stub -Tag $Tag

	Write-Host 'Part 4: --version reports a -dev suffix on the resolved version' -ForegroundColor Cyan
	$stub = New-Stub -Name 'fail_dev' -Body 'echo podup version v3.7.0-dev'
	Invoke-Fail -StubPath $stub -Tag $Tag

	Write-Host 'Part 5: --version reports garbage' -ForegroundColor Cyan
	$stub = New-Stub -Name 'fail_garbage' -Body 'echo definitely not a podup'
	Invoke-Fail -StubPath $stub -Tag $Tag

	Write-Host 'Part 6: --version exits non-zero' -ForegroundColor Cyan
	$stub = New-Stub -Name 'fail_exit' -Body 'echo podup version v3.7.0' -ExitCode '1'
	Invoke-Fail -StubPath $stub -Tag $Tag

	Write-Host 'Part 7: staged file does not exist' -ForegroundColor Cyan
	Invoke-Fail -StubPath (Join-Path $stubsDir "does-not-exist$stubExt") -Tag $Tag

	Write-Host ''
	Write-Host 'All parts passed.' -ForegroundColor Green
} finally {
	Remove-Item -LiteralPath $stubsDir -Recurse -Force -ErrorAction SilentlyContinue
}