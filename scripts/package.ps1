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
$commit = (& git -C $build.RepositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Unable to bind the package to an immutable source commit.'
}
$metadata = [ordered]@{
    schema_version = 1
    application_version = '6.0.0'
    protocol_version = 1
    commit = $commit
    platform = 'windows-x64'
}
$metadataJson = $metadata | ConvertTo-Json -Depth 4
[System.IO.File]::WriteAllText(
    (Join-Path $build.Paths.RendererDist 'build-metadata.json'),
    $metadataJson + [Environment]::NewLine,
    [System.Text.UTF8Encoding]::new($false)
)

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
& (Join-Path $PSScriptRoot 'harden-package.ps1') -SkipSpaceCheck:$SkipSpaceCheck
Write-Warning 'Artifacts are unsigned candidates until a signing certificate is configured and verified.'
