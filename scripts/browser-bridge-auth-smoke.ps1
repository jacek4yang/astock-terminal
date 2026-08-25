[CmdletBinding()]
param([switch]$SkipSpaceCheck)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
Set-AStockWorkerEnvironment -Environment $build -Release

$isolatedData = Join-Path $build.Paths.FormalCache "bridge-auth-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $isolatedData -Force | Out-Null
$previousData = $env:ASTOCK_DATA_DIR
$process = $null
$client = $null

function Send-BridgeRequest {
    param(
        [Parameter(Mandatory)][System.Net.Http.HttpClient]$Client,
        [Parameter(Mandatory)][string]$BaseUrl,
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Token,
        [Parameter(Mandatory)][string]$Origin
    )
    $request = [System.Net.Http.HttpRequestMessage]::new(
        [System.Net.Http.HttpMethod]::new($Method),
        $BaseUrl + $Path
    )
    $null = $request.Headers.TryAddWithoutValidation('Origin', $Origin)
    $null = $request.Headers.TryAddWithoutValidation('X-AStock-Test-Token', $Token)
    try {
        return $Client.SendAsync($request).GetAwaiter().GetResult()
    } finally {
        $request.Dispose()
    }
}

try {
    $env:ASTOCK_DATA_DIR = $isolatedData
    $start = [System.Diagnostics.ProcessStartInfo]::new()
    $start.FileName = (Get-Command node).Source
    $start.ArgumentList.Add((Join-Path $PSScriptRoot 'browser-dev-bridge.mjs'))
    $start.ArgumentList.Add($env:ASTOCK_ENGINE_EXE)
    $start.ArgumentList.Add($env:ASTOCK_AGENT_EXE)
    $start.WorkingDirectory = $build.RepositoryRoot
    $start.UseShellExecute = $false
    $start.CreateNoWindow = $true
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = [System.Diagnostics.Process]::Start($start)
    if (-not $process) { throw 'Unable to start the development browser Bridge.' }
    $readyLine = $process.StandardOutput.ReadLine()
    if ([string]::IsNullOrWhiteSpace($readyLine)) {
        throw "Development browser Bridge did not become ready: $($process.StandardError.ReadToEnd())"
    }
    $ready = $readyLine | ConvertFrom-Json
    $fragment = ([Uri]$ready.ui_url).Fragment.TrimStart('#')
    $fragmentValues = @{}
    foreach ($pair in $fragment.Split('&')) {
        $parts = $pair.Split('=', 2)
        if ($parts.Count -eq 2) {
            $fragmentValues[[Uri]::UnescapeDataString($parts[0])] = [Uri]::UnescapeDataString($parts[1])
        }
    }
    $bootstrap = [string]$fragmentValues.bridgeToken
    if ($bootstrap.Length -lt 32) { throw 'Development browser Bridge did not emit a bootstrap token.' }

    $handler = [System.Net.Http.SocketsHttpHandler]::new()
    $handler.UseProxy = $false
    $client = [System.Net.Http.HttpClient]::new($handler, $true)
    $first = Send-BridgeRequest $client $ready.bridge_url 'POST' '/session' $bootstrap 'http://127.0.0.1:5173'
    try {
        if ([int]$first.StatusCode -ne 200) { throw "First bootstrap exchange failed: $([int]$first.StatusCode)" }
        $sessionPayload = $first.Content.ReadAsStringAsync().GetAwaiter().GetResult() | ConvertFrom-Json
    } finally {
        $first.Dispose()
    }
    $session = [string]$sessionPayload.session_token
    if ($session.Length -lt 32 -or $session -eq $bootstrap) { throw 'Development browser Bridge returned an invalid session token.' }

    $replay = Send-BridgeRequest $client $ready.bridge_url 'POST' '/session' $bootstrap 'http://127.0.0.1:5173'
    try { $replayStatus = [int]$replay.StatusCode } finally { $replay.Dispose() }
    $health = Send-BridgeRequest $client $ready.bridge_url 'GET' '/health' $session 'http://127.0.0.1:5173'
    try { $healthStatus = [int]$health.StatusCode } finally { $health.Dispose() }
    $wrongOrigin = Send-BridgeRequest $client $ready.bridge_url 'GET' '/health' $session 'http://malicious.invalid'
    try { $wrongOriginStatus = [int]$wrongOrigin.StatusCode } finally { $wrongOrigin.Dispose() }

    if ($replayStatus -ne 401 -or $healthStatus -ne 200 -or $wrongOriginStatus -ne 401) {
        throw "Bridge authorization mismatch: replay=$replayStatus health=$healthStatus wrong_origin=$wrongOriginStatus"
    }
    Write-Output 'Bridge one-time bootstrap: replay=401; session-health=200; wrong-origin=401'
} finally {
    if ($client) { $client.Dispose() }
    if ($process -and -not $process.HasExited) {
        $process.Kill($true)
        $process.WaitForExit(5000) | Out-Null
    }
    if ($null -eq $previousData) { Remove-Item Env:ASTOCK_DATA_DIR -ErrorAction SilentlyContinue }
    else { $env:ASTOCK_DATA_DIR = $previousData }
}
