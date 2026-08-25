[CmdletBinding()]
param(
    [switch]$SkipSpaceCheck,
    [switch]$SkipProtonCli,
    [switch]$SkipCef,
    [switch]$SkipGitleaks
)

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck

if (-not $SkipProtonCli) {
    $protonCli = Join-Path $build.Paths.Tools 'proton_cli.exe'
    if (-not (Test-Path -LiteralPath $protonCli -PathType Leaf)) {
        Push-Location $build.RepositoryRoot
        try {
            Invoke-Checked -FilePath 'moon' -Arguments @(
                'install',
                'moonbit-community/proton_cli@0.2.1',
                '--bin', $build.Paths.Tools,
                '--target-dir', $build.Paths.MoonTools
            )
        } finally {
            Pop-Location
        }
    }
}

if (-not $SkipCef) {
    if ($env:OS -eq 'Windows_NT') {
        & (Join-Path $PSScriptRoot 'install-pinned-cef-runtime.ps1') `
            -RuntimeStore $build.Paths.ProtonRuntime `
            -TempRoot $build.Paths.Temp
    }
    $protonCli = Get-ProtonCliPath -Environment $build
    Invoke-Checked -FilePath $protonCli -WorkingDirectory (Join-Path $build.RepositoryRoot 'desktop-moon') -PreserveProxy -Arguments @(
        'cef', 'setup'
    )
}

if (-not $SkipGitleaks) {
    $gitleaks = Join-Path $build.Paths.Tools 'gitleaks.exe'
    if (-not (Test-Path -LiteralPath $gitleaks -PathType Leaf)) {
        $version = '8.30.1'
        $archive = Join-Path $build.Paths.Temp "gitleaks-$version-$([Guid]::NewGuid().ToString('N')).zip"
        $extract = Join-Path $build.Paths.Temp "gitleaks-$version-$([Guid]::NewGuid().ToString('N'))"
        Invoke-WebRequest `
            -Uri "https://github.com/gitleaks/gitleaks/releases/download/v$version/gitleaks_${version}_windows_x64.zip" `
            -OutFile $archive `
            -UseBasicParsing
        $archiveHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash.ToLowerInvariant()
        if ($archiveHash -ne 'd29144deff3a68aa93ced33dddf84b7fdc26070add4aa0f4513094c8332afc4e') {
            throw "Pinned Gitleaks archive digest mismatch: $archiveHash"
        }
        New-Item -ItemType Directory -Path $extract -Force | Out-Null
        Expand-Archive -LiteralPath $archive -DestinationPath $extract
        $executable = Join-Path $extract 'gitleaks.exe'
        if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
            throw 'Pinned Gitleaks archive does not contain gitleaks.exe.'
        }
        Copy-Item -LiteralPath $executable -Destination $gitleaks
    }
    $gitleaksHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $gitleaks).Hash.ToLowerInvariant()
    if ($gitleaksHash -ne '17157e2ee8b76fc8b1d8bee607a250e34b8a8023c8bc81822d4b5ee4d78fcb7c') {
        throw "Pinned Gitleaks executable digest mismatch: $gitleaksHash"
    }
    $gitleaksVersion = (& $gitleaks version).Trim()
    if ($LASTEXITCODE -ne 0 -or $gitleaksVersion -ne '8.30.1') {
        throw "Pinned Gitleaks version mismatch: $gitleaksVersion"
    }
}

Enable-AStockProtonAlloyRuntime -RepositoryRoot $build.RepositoryRoot

$arAlias = Join-Path $build.Paths.Tools 'ar.exe'
if (-not (Test-Path -LiteralPath $arAlias -PathType Leaf)) {
    $llvmAr = (Get-Command 'llvm-ar.exe' -ErrorAction SilentlyContinue).Source
    if ([string]::IsNullOrWhiteSpace($llvmAr)) {
        throw 'LLVM archiver is required. Install LLVM and rerun bootstrap.'
    }
    Copy-Item -LiteralPath $llvmAr -Destination $arAlias
}

Write-Host "AStock build root: $($build.Paths.Root)"
Write-Host "Cargo target:     $($build.Paths.Cargo)"
Write-Host "CEF runtime:      $($build.Paths.ProtonRuntime)"
Write-Host "Artifacts:        $($build.Paths.Artifacts)"
if (-not $SkipGitleaks) { Write-Host "Gitleaks:         $(Join-Path $build.Paths.Tools 'gitleaks.exe')" }
