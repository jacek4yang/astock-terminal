[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$InstallRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$expectedMoonVersion = '0.1.20260824'
$expectedMoonCommit = 'dae026a'
$toolchainUrl = 'https://cli.moonbitlang.com/binaries/latest/moonbit-windows-x86_64.zip'
$toolchainSha256 = '915a560cc4950a124bfedf5302ec6bf0d0f98d8ea6b2ae7978e4680641281963'
$coreUrl = 'https://cli.moonbitlang.com/cores/core-latest.zip'
$coreSha256 = 'ca33c246472d02ce3805f8fc96b20e1819bf530f2fca7fe6610f5c9a601ee6eb'

if ([string]::IsNullOrWhiteSpace($env:ASTOCK_BUILD_ROOT)) {
    throw 'ASTOCK_BUILD_ROOT is required for the CI MoonBit bootstrap.'
}
$buildRoot = [System.IO.Path]::GetFullPath($env:ASTOCK_BUILD_ROOT)
$install = [System.IO.Path]::GetFullPath($InstallRoot)
$buildPrefix = $buildRoot.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
if (-not $install.StartsWith($buildPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "MoonBit CI toolchain must be installed below ASTOCK_BUILD_ROOT: $install"
}

$downloadRoot = Join-Path $buildRoot 'temp\moonbit-ci'
$toolchainArchive = Join-Path $downloadRoot 'moonbit-windows-x86_64.zip'
$coreArchive = Join-Path $downloadRoot 'core-latest.zip'
New-Item -ItemType Directory -Path $downloadRoot -Force | Out-Null

function Get-PinnedArchive {
    param(
        [Parameter(Mandatory)][string]$Uri,
        [Parameter(Mandatory)][string]$Destination,
        [Parameter(Mandatory)][string]$ExpectedSha256
    )
    if (-not (Test-Path -LiteralPath $Destination -PathType Leaf)) {
        Invoke-WebRequest -Uri $Uri -OutFile $Destination -MaximumRetryCount 5 -RetryIntervalSec 2 -UseBasicParsing
    }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Destination).Hash.ToLowerInvariant()
    if ($actual -ne $ExpectedSha256) {
        throw "Pinned MoonBit archive digest mismatch for ${Destination}: $actual"
    }
}

Get-PinnedArchive -Uri $toolchainUrl -Destination $toolchainArchive -ExpectedSha256 $toolchainSha256
Get-PinnedArchive -Uri $coreUrl -Destination $coreArchive -ExpectedSha256 $coreSha256

if (Test-Path -LiteralPath $install) {
    $existing = @(Get-ChildItem -LiteralPath $install -Force -ErrorAction Stop)
    if ($existing.Count -ne 0) {
        throw "MoonBit CI install root must be new and empty: $install"
    }
} else {
    New-Item -ItemType Directory -Path $install -Force | Out-Null
}

Expand-Archive -LiteralPath $toolchainArchive -DestinationPath $install -Force
$moonBin = Join-Path $install 'bin'
$moonExe = Join-Path $moonBin 'moon.exe'
$moonxExe = Join-Path $moonBin 'moonx.exe'
if (-not (Test-Path -LiteralPath $moonExe -PathType Leaf)) {
    throw "MoonBit archive does not contain moon.exe: $toolchainArchive"
}
if (-not (Test-Path -LiteralPath $moonxExe -PathType Leaf)) {
    New-Item -ItemType HardLink -Path $moonxExe -Target $moonExe | Out-Null
}

$moonLib = Join-Path $install 'lib'
Expand-Archive -LiteralPath $coreArchive -DestinationPath $moonLib -Force
$coreRoot = Join-Path $moonLib 'core'
if (-not (Test-Path -LiteralPath (Join-Path $coreRoot 'moon.mod') -PathType Leaf)) {
    throw "MoonBit core archive is incomplete: $coreArchive"
}

$env:MOON_HOME = $install
$env:PATH = "$moonBin;$env:PATH"
Push-Location $coreRoot
try {
    & $moonExe bundle --warn-list -a --all
    if ($LASTEXITCODE -ne 0) { throw 'MoonBit core bundle failed.' }
    & $moonExe bundle --warn-list -a --target wasm-gc --quiet
    if ($LASTEXITCODE -ne 0) { throw 'MoonBit wasm-gc core bundle failed.' }
} finally {
    Pop-Location
}

$versionOutput = (& $moonExe version --all | Out-String)
if ($LASTEXITCODE -ne 0 -or
    -not $versionOutput.Contains("moon $expectedMoonVersion ($expectedMoonCommit") -or
    -not $versionOutput.Contains('moonc v0.10.10+f8a486b6f')) {
    throw "MoonBit CI toolchain identity mismatch:`n$versionOutput"
}

$manifest = [ordered]@{
    schema_version = 1
    moon_version = $expectedMoonVersion
    moon_commit = $expectedMoonCommit
    moonc_version = '0.10.10+f8a486b6f'
    toolchain_url = $toolchainUrl
    toolchain_sha256 = $toolchainSha256
    core_url = $coreUrl
    core_sha256 = $coreSha256
}
$manifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $install 'astock-toolchain-manifest.json') -Encoding utf8NoBOM

if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_ENV)) {
    "MOON_HOME=$install" | Out-File -LiteralPath $env:GITHUB_ENV -Encoding utf8 -Append
}
if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_PATH)) {
    $moonBin | Out-File -LiteralPath $env:GITHUB_PATH -Encoding utf8 -Append
}

Write-Host $versionOutput.Trim()
Write-Host "Pinned MoonBit CI toolchain: $install"
