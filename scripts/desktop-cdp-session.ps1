[CmdletBinding()]
param(
    [string]$PackageDirectory,
    [ValidateSet('smoke','renderer-fault')]
    [string]$Mode = 'smoke',
    [switch]$Headless = $true,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$commit = (& git -C $build.RepositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') { throw 'Unable to resolve the desktop CDP source commit.' }
if ([string]::IsNullOrWhiteSpace($PackageDirectory)) {
    $PackageDirectory = Join-Path $build.Paths.Artifacts 'astock-terminal'
}
$packageRoot = [System.IO.Path]::GetFullPath($PackageDirectory)
$host = Join-Path $packageRoot 'astock-terminal.exe'
$metadataPath = Join-Path $packageRoot 'Resources\build-metadata.json'
foreach ($required in @($host, $metadataPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) { throw "Packaged CDP input is missing: $required" }
}
$metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
if ($metadata.commit -ne $commit -or $metadata.application_version -ne '6.0.0') {
    throw 'Packaged CDP input is not bound to the current v6.0.0 commit.'
}

$listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
$listener.Start()
$port = ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
$listener.Stop()
$runRoot = Join-Path $build.Paths.FormalCache "desktop-cdp-$($commit.Substring(0, 12))-$([Guid]::NewGuid().ToString('N'))"
$dataRoot = Join-Path $runRoot 'data'
$localAppData = Join-Path $runRoot 'local'
$appData = Join-Path $runRoot 'roaming'
New-Item -ItemType Directory -Path $dataRoot,$localAppData,$appData -Force | Out-Null

$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $host
$startInfo.WorkingDirectory = $packageRoot
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $true
$startInfo.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
$startInfo.Environment['ASTOCK_DATA_DIR'] = $dataRoot
$startInfo.Environment['ASTOCK_RELEASE_TEST_CDP'] = '1'
$startInfo.Environment['PROTON_REMOTE_DEBUGGING_PORT'] = [string]$port
$startInfo.Environment['LOCALAPPDATA'] = $localAppData
$startInfo.Environment['APPDATA'] = $appData
if ($Headless) {
    $startInfo.Environment['PROTON_HEADLESS'] = '1'
    $startInfo.Environment['PROTON_DISABLE_GPU'] = '1'
}
$process = [System.Diagnostics.Process]::Start($startInfo)
if (-not $process) { throw 'Unable to launch the packaged Proton/CEF application.' }

try {
    $runner = if ($Mode -eq 'renderer-fault') { 'desktop-renderer-fault.mjs' } else { 'desktop-cdp-smoke.mjs' }
    $output = & node (Join-Path $PSScriptRoot $runner) $port $commit
    if ($LASTEXITCODE -ne 0) {
        if ($process.HasExited) { throw "Packaged desktop exited before CDP verification (exit $($process.ExitCode)); another single-instance window may already be running." }
        throw 'Packaged desktop CDP smoke failed.'
    }
    ($output -join "`n") | ConvertFrom-Json | ConvertTo-Json -Depth 8
} finally {
    if (-not $process.HasExited) {
        if (-not $process.WaitForExit(5000)) { $process.Kill($true) }
    }
}
