[CmdletBinding()]
param(
    [switch]$SkipSpaceCheck,
    [switch]$SkipProtonCli,
    [switch]$SkipCef
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
    $protonCli = Get-ProtonCliPath -Environment $build
    Invoke-Checked -FilePath $protonCli -WorkingDirectory (Join-Path $build.RepositoryRoot 'desktop-moon') -PreserveProxy -Arguments @(
        'cef', 'setup'
    )
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
