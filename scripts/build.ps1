[CmdletBinding()]
param(
    [ValidateSet('all', 'ui', 'engine', 'agent', 'desktop')]
    [string]$Component = 'all',
    [switch]$Release,
    [switch]$SkipSpaceCheck
)

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$cargoArgs = @('build', '--locked', '-p', 'astock-engine')
if ($Release) { $cargoArgs += '--release' }
$moonMode = if ($Release) { '--release' } else { '--debug' }

Push-Location $build.RepositoryRoot
try {
    if ($Component -in @('all', 'ui')) {
        Invoke-Checked -FilePath 'npm' -Arguments @('--prefix', 'ui', 'run', 'build')
    }
    if ($Component -in @('all', 'engine')) {
        Invoke-Checked -FilePath 'cargo' -Arguments $cargoArgs
    }
    if ($Component -in @('all', 'agent')) {
        Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'app-moon') -Arguments @(
            'build', '--target', 'native', $moonMode,
            '--target-dir', $build.Paths.MoonAgent,
            'agent_worker'
        )
    }
    if ($Component -in @('all', 'desktop')) {
        $protonCli = Get-ProtonCliPath -Environment $build
        Set-AStockWorkerEnvironment -Environment $build -Release:$Release
        $protonConfig = New-AStockProtonConfig -Environment $build
        $protonArgs = @('build', '--config', $protonConfig, '--moon-target-dir', $build.Paths.MoonDesktop)
        if ($Component -eq 'all') { $protonArgs += '--no-frontend' }
        if ($Release) { $protonArgs += @('--', '--release') }
        Invoke-Checked -FilePath $protonCli -WorkingDirectory (Join-Path $build.RepositoryRoot 'desktop-moon') -Arguments $protonArgs
    }
} finally {
    Pop-Location
}
