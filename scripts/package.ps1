[CmdletBinding()]
param([switch]$SkipSpaceCheck)

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck

& (Join-Path $PSScriptRoot 'build.ps1') -Component all -Release -SkipSpaceCheck:$SkipSpaceCheck
Set-AStockWorkerEnvironment -Environment $build -Release

$workerStage = Join-Path $build.Paths.RendererDist 'workers'
New-Item -ItemType Directory -Path $workerStage -Force | Out-Null
Copy-Item -LiteralPath $env:ASTOCK_ENGINE_EXE -Destination (Join-Path $workerStage 'astock-engine.exe') -Force
Copy-Item -LiteralPath $env:ASTOCK_AGENT_EXE -Destination (Join-Path $workerStage 'astock-agent.exe') -Force

$hostExe = Join-Path $build.Paths.MoonDesktop 'native\release\build\astock\desktop_backend\app\app.exe'
if (-not (Test-Path -LiteralPath $hostExe -PathType Leaf)) {
    throw "Prebuilt Proton Host is missing: $hostExe"
}
$env:ASTOCK_HOST_EXE = $hostExe
$env:ASTOCK_DESKTOP_ROOT = Join-Path $build.RepositoryRoot 'desktop-moon'
$env:ASTOCK_MOON_DESKTOP_TARGET = $build.Paths.MoonDesktop

# The adapter calls proton_bundle/proton_package on the prebuilt executable;
# it never invokes Proton's project-local package rebuild path.
Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $build.RepositoryRoot 'packaging-moon') -Arguments @(
    'run', '--target', 'native', '--release', '--target-dir', $build.Paths.MoonTools, 'packager'
)
Write-Warning 'Artifacts are unsigned candidates until a signing certificate is configured and verified.'
