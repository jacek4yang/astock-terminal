[CmdletBinding()]
param([switch]$SkipSpaceCheck)

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$protonCli = Get-ProtonCliPath -Environment $build
$protonConfig = New-AStockProtonConfig -Environment $build

Invoke-Checked -FilePath 'cargo' -WorkingDirectory $build.RepositoryRoot -Arguments @('build', '--locked', '-p', 'astock-engine')
Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'app-moon') -Arguments @(
    'build', '--target', 'native', '--debug', '--target-dir', $build.Paths.MoonAgent, 'agent_worker'
)
Set-AStockWorkerEnvironment -Environment $build

Invoke-Checked -FilePath $protonCli -WorkingDirectory (Join-Path $build.RepositoryRoot 'desktop-moon') -Arguments @(
    'dev', '--config', $protonConfig, '--moon-target-dir', $build.Paths.MoonDesktop
)
