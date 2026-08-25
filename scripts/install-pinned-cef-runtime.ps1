[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$RuntimeStore,
    [Parameter(Mandatory)][string]$TempRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$cefArchiveName = 'cef_binary_147.0.14+g76d2442+chromium-147.0.7727.138_windows64_minimal'
$cefSha256 = 'c105f69c0d4dc14331be12cee967eeb73fa4897a6ecde244051318491cd381c7'
$layoutVersion = 1
$RuntimeStore = [System.IO.Path]::GetFullPath($RuntimeStore)
$TempRoot = [System.IO.Path]::GetFullPath($TempRoot)
$runtimeId = "cef-$cefSha256-layout-$layoutVersion"
$runtimeRoot = Join-Path $RuntimeStore "win32-x64\$runtimeId"
$manifestPath = Join-Path $runtimeRoot 'manifest.json'

function Assert-ContainedPath {
    param(
        [Parameter(Mandatory)][string]$Candidate,
        [Parameter(Mandatory)][string]$Parent,
        [Parameter(Mandatory)][string]$Label
    )

    $candidateFull = [System.IO.Path]::GetFullPath($Candidate)
    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\') + '\'
    if (-not $candidateFull.StartsWith($parentFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "$Label must stay under $Parent; got $candidateFull"
    }
    return $candidateFull
}

function Test-PinnedRuntime {
    if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { return $false }
    try {
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    } catch {
        return $false
    }
    if ($manifest.schema_version -ne 1 -or
        $manifest.platform -ne 'win32-x64' -or
        $manifest.cef_archive -ne $cefArchiveName -or
        $manifest.cef_sha256 -ne $cefSha256 -or
        $manifest.layout_version -ne $layoutVersion -or
        $manifest.sdk -ne 'sdk' -or
        $manifest.runtime -ne 'runtime') {
        return $false
    }

    $required = @(
        'sdk\include\base\cef_build.h',
        'sdk\include\capi\cef_base_capi.h',
        'sdk\include\capi\cef_app_capi.h',
        'sdk\include\capi\cef_browser_capi.h',
        'sdk\include\capi\cef_client_capi.h',
        'sdk\include\capi\cef_v8_capi.h',
        'sdk\Release\libcef.lib',
        'sdk\Release\libcef.dll',
        'sdk\Resources\icudtl.dat',
        'sdk\Release\icudtl.dat',
        'sdk\Release\chrome_100_percent.pak',
        'sdk\Release\chrome_200_percent.pak',
        'sdk\Release\resources.pak',
        'runtime\bin\libcef.dll',
        'runtime\bin\icudtl.dat',
        'runtime\Resources\icudtl.dat',
        'runtime\Resources\locales'
    )
    foreach ($relativePath in $required) {
        if (-not (Test-Path -LiteralPath (Join-Path $runtimeRoot $relativePath))) {
            return $false
        }
    }
    return $true
}

if ($env:OS -ne 'Windows_NT') {
    throw 'The pinned fast CEF installer currently supports Windows x64 only.'
}
if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'The pinned CEF runtime requires Windows x64.'
}

New-Item -ItemType Directory -Path $RuntimeStore -Force | Out-Null
New-Item -ItemType Directory -Path $TempRoot -Force | Out-Null

if (Test-PinnedRuntime) {
    Write-Host "Pinned CEF runtime is already complete: $runtimeRoot"
    return
}
if (Test-Path -LiteralPath $runtimeRoot) {
    throw "Pinned CEF runtime exists but failed validation; refusing to replace it automatically: $runtimeRoot"
}

$sevenZipCommand = Get-Command '7z.exe' -ErrorAction SilentlyContinue
$sevenZip = if ($null -ne $sevenZipCommand) {
    $sevenZipCommand.Source
} else {
    $candidates = @()
    if (-not [string]::IsNullOrWhiteSpace($env:ProgramFiles)) {
        $candidates += Join-Path $env:ProgramFiles '7-Zip\7z.exe'
    }
    if (-not [string]::IsNullOrWhiteSpace($env:ChocolateyInstall)) {
        $candidates += Join-Path $env:ChocolateyInstall 'bin\7z.exe'
    }
    $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
}
if ([string]::IsNullOrWhiteSpace($sevenZip)) {
    throw '7-Zip is required for bounded Windows CEF extraction.'
}

$archiveDirectory = Join-Path $RuntimeStore 'downloads'
New-Item -ItemType Directory -Path $archiveDirectory -Force | Out-Null
$archivePath = Join-Path $archiveDirectory "$cefArchiveName.tar.bz2"
if (Test-Path -LiteralPath $archivePath -PathType Leaf) {
    $cachedHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archivePath).Hash.ToLowerInvariant()
    if ($cachedHash -ne $cefSha256) {
        throw "Cached CEF archive digest mismatch: expected $cefSha256, got $cachedHash"
    }
} else {
    $partialPath = Assert-ContainedPath `
        -Candidate (Join-Path $archiveDirectory "$cefArchiveName.$([Guid]::NewGuid().ToString('N')).partial") `
        -Parent $archiveDirectory `
        -Label 'CEF partial download'
    try {
        & curl.exe -L --fail --retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 30 `
            --output $partialPath "https://cef-builds.spotifycdn.com/$cefArchiveName.tar.bz2"
        if ($LASTEXITCODE -ne 0) { throw "CEF download failed with exit code $LASTEXITCODE" }
        $downloadHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $partialPath).Hash.ToLowerInvariant()
        if ($downloadHash -ne $cefSha256) {
            throw "CEF archive digest mismatch: expected $cefSha256, got $downloadHash"
        }
        Move-Item -LiteralPath $partialPath -Destination $archivePath
    } finally {
        if (Test-Path -LiteralPath $partialPath -PathType Leaf) {
            Remove-Item -LiteralPath $partialPath -Force
        }
    }
}

$workRoot = Assert-ContainedPath `
    -Candidate (Join-Path $TempRoot "cef-fast-$([Guid]::NewGuid().ToString('N'))") `
    -Parent $TempRoot `
    -Label 'CEF extraction directory'
$runtimeParent = Join-Path $RuntimeStore 'win32-x64'
New-Item -ItemType Directory -Path $runtimeParent -Force | Out-Null
$stagingRoot = Assert-ContainedPath `
    -Candidate (Join-Path $runtimeParent ".$runtimeId.stage-$([Guid]::NewGuid().ToString('N'))") `
    -Parent $runtimeParent `
    -Label 'CEF runtime staging directory'

try {
    New-Item -ItemType Directory -Path $workRoot | Out-Null
    Write-Host "Extracting pinned CEF with 7-Zip: $archivePath"
    & $sevenZip x $archivePath "-o$workRoot" -y
    if ($LASTEXITCODE -ne 0) { throw "7-Zip bzip2 extraction failed with exit code $LASTEXITCODE" }
    $tarPath = Join-Path $workRoot "$cefArchiveName.tar"
    if (-not (Test-Path -LiteralPath $tarPath -PathType Leaf)) {
        throw "7-Zip did not produce the expected tar archive: $tarPath"
    }
    & $sevenZip x $tarPath "-o$workRoot" -y
    if ($LASTEXITCODE -ne 0) { throw "7-Zip tar extraction failed with exit code $LASTEXITCODE" }

    $extractedRoot = Join-Path $workRoot $cefArchiveName
    if (-not (Test-Path -LiteralPath (Join-Path $extractedRoot 'Release\libcef.dll') -PathType Leaf)) {
        throw "Extracted CEF root is incomplete: $extractedRoot"
    }

    New-Item -ItemType Directory -Path $stagingRoot | Out-Null
    $sdkRoot = Join-Path $stagingRoot 'sdk'
    $runtimeDist = Join-Path $stagingRoot 'runtime'
    Copy-Item -LiteralPath $extractedRoot -Destination $sdkRoot -Recurse

    $sdkRelease = Join-Path $sdkRoot 'Release'
    foreach ($fileName in @('icudtl.dat', 'chrome_100_percent.pak', 'chrome_200_percent.pak', 'resources.pak')) {
        $source = Join-Path (Join-Path $sdkRoot 'Resources') $fileName
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination (Join-Path $sdkRelease $fileName)
        }
    }

    New-Item -ItemType Directory -Path $runtimeDist | Out-Null
    Copy-Item -LiteralPath $sdkRelease -Destination (Join-Path $runtimeDist 'bin') -Recurse
    Copy-Item -LiteralPath (Join-Path $sdkRoot 'Resources') -Destination (Join-Path $runtimeDist 'Resources') -Recurse

    $manifest = [ordered]@{
        schema_version = 1
        platform = 'win32-x64'
        cef_archive = $cefArchiveName
        cef_sha256 = $cefSha256
        layout_version = $layoutVersion
        sdk = 'sdk'
        runtime = 'runtime'
    }
    $manifest | ConvertTo-Json -Compress | Set-Content -LiteralPath (Join-Path $stagingRoot 'manifest.json') -Encoding utf8NoBOM

    Move-Item -LiteralPath $stagingRoot -Destination $runtimeRoot
    if (-not (Test-PinnedRuntime)) {
        throw "Pinned CEF runtime failed post-install validation: $runtimeRoot"
    }
    Write-Host "Pinned CEF runtime installed with verified archive digest: $runtimeRoot"
} finally {
    foreach ($temporaryPath in @($workRoot, $stagingRoot)) {
        if (Test-Path -LiteralPath $temporaryPath) {
            Remove-Item -LiteralPath $temporaryPath -Recurse -Force
        }
    }
}
