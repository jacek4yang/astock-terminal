[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$SessionDirectory,
    [string]$ExpectedCommit = '',
    [string]$CodexPackageFamily = 'OpenAI.Codex_2p2nqsd0c76g0',
    [switch]$AllowBlocked,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck

function Test-NoProxyHost {
    param(
        [Parameter(Mandatory)][AllowEmptyCollection()][string[]]$Entries,
        [Parameter(Mandatory)][string]$HostName
    )

    foreach ($entry in $Entries) {
        $candidate = $entry.Trim()
        if ($candidate -eq '*') { return $true }
        if ($candidate.StartsWith('[') -and $candidate.Contains(']')) {
            $candidate = $candidate.Substring(0, $candidate.IndexOf(']') + 1)
        } elseif ($candidate.Contains(':')) {
            $candidate = $candidate.Split(':')[0]
        }
        if ($candidate.TrimStart('.').Equals($HostName, [StringComparison]::OrdinalIgnoreCase)) {
            return $true
        }
    }
    return $false
}

function Get-ProcessAncestorNames {
    $names = [System.Collections.Generic.List[string]]::new()
    $current = Get-CimInstance Win32_Process -Filter "ProcessId=$PID"
    for ($depth = 0; $depth -lt 16 -and $null -ne $current; $depth++) {
        if (-not [string]::IsNullOrWhiteSpace($current.Name)) {
            $names.Add($current.Name.ToLowerInvariant())
        }
        if (-not $current.ParentProcessId) { break }
        $current = Get-CimInstance Win32_Process `
            -Filter "ProcessId=$($current.ParentProcessId)" `
            -ErrorAction SilentlyContinue
    }
    return @($names | Select-Object -Unique)
}

$sessionRoot = [System.IO.Path]::GetFullPath($SessionDirectory)
$buildPrefix = $build.Paths.Root.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
if (-not $sessionRoot.StartsWith($buildPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Browser acceptance session must be a child of ASTOCK_BUILD_ROOT: $sessionRoot"
}
$sessionPath = Join-Path $sessionRoot 'session.json'
if (-not (Test-Path -LiteralPath $sessionPath -PathType Leaf)) {
    throw "Browser acceptance session is missing session.json: $sessionRoot"
}
$session = Get-Content -LiteralPath $sessionPath -Raw | ConvertFrom-Json
if ($session.schema_version -ne 1 -or $session.mode -ne 'browser' -or $session.surface -ne 'codex-in-app-browser') {
    throw 'Browser acceptance session policy is invalid.'
}
if ($session.commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Browser acceptance session commit is invalid.'
}
if (-not [string]::IsNullOrWhiteSpace($ExpectedCommit) -and $session.commit -ne $ExpectedCommit) {
    throw "Browser acceptance session commit does not match $ExpectedCommit."
}

$outputPath = Join-Path $sessionRoot 'browser-environment-preflight.json'
if (Test-Path -LiteralPath $outputPath) {
    throw "Browser environment preflight evidence already exists: $outputPath"
}

$proxyNames = @('ALL_PROXY', 'all_proxy', 'HTTP_PROXY', 'http_proxy', 'HTTPS_PROXY', 'https_proxy')
$configuredProxyNames = @($proxyNames | Where-Object {
    -not [string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($_, 'Process'))
} | Select-Object -Unique)
$noProxyValues = @(
    [Environment]::GetEnvironmentVariable('NO_PROXY', 'Process'),
    [Environment]::GetEnvironmentVariable('no_proxy', 'Process')
) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
$noProxyEntries = @($noProxyValues -split '[,;]' | ForEach-Object { $_.Trim() } | Where-Object { $_ })
$bypass127 = Test-NoProxyHost -Entries $noProxyEntries -HostName '127.0.0.1'
$bypassLocalhost = Test-NoProxyHost -Entries $noProxyEntries -HostName 'localhost'
$proxyBypassReady = $configuredProxyNames.Count -eq 0 -or ($bypass127 -and $bypassLocalhost)

$loopbackListing = (& CheckNetIsolation LoopbackExempt -s 2>$null | Out-String)
$loopbackExempt = $LASTEXITCODE -eq 0 -and $loopbackListing.Contains($CodexPackageFamily)
$ancestorNames = Get-ProcessAncestorNames
$codexProcessAncestor = $ancestorNames -contains 'codex.exe' -or $ancestorNames -contains 'chatgpt.exe'
$status = if ($loopbackExempt -and $proxyBypassReady -and $codexProcessAncestor) { 'PASSED' } else { 'BLOCKED' }
$remediation = [System.Collections.Generic.List[string]]::new()
if (-not $loopbackExempt) {
    $remediation.Add("Require explicit operator approval before: CheckNetIsolation LoopbackExempt -a -n=$CodexPackageFamily")
}
if (-not $proxyBypassReady) {
    $remediation.Add('Add 127.0.0.1 and localhost to the Codex process proxy bypass, then restart Codex before acceptance.')
}
if (-not $codexProcessAncestor) {
    $remediation.Add('Run this preflight from the Codex desktop task that will control the in-app browser.')
}

$completed = [DateTime]::UtcNow.ToString('o')
$evidence = [ordered]@{
    schema_version = 1
    gate = 'browser-environment-preflight'
    status = $status
    commit = $session.commit
    session_id = $session.session_id
    started_at_utc = $session.started_at_utc
    completed_at_utc = $completed
    surface = 'codex-in-app-browser'
    test_origin = 'http://127.0.0.1:5173/'
    codex_process_ancestor = $codexProcessAncestor
    process_ancestor_names = $ancestorNames
    loopback_exempt = $loopbackExempt
    proxy_environment_variables = $configuredProxyNames
    no_proxy_127_0_0_1 = $bypass127
    no_proxy_localhost = $bypassLocalhost
    proxy_bypass_ready = $proxyBypassReady
    browser_navigation_tested = $false
    environment_preflight_only = $true
    production_data_touched = $false
    secrets_in_evidence = $false
    remediation = @($remediation)
}
$json = $evidence | ConvertTo-Json -Depth 8
[System.IO.File]::WriteAllText($outputPath, "$json`n", [System.Text.UTF8Encoding]::new($false))

Write-Host "Browser acceptance preflight: $status"
Write-Host "Evidence: $outputPath"
Write-Host "Loopback exempt: $loopbackExempt; proxy bypass ready: $proxyBypassReady; Codex ancestor: $codexProcessAncestor"
if ($status -ne 'PASSED' -and -not $AllowBlocked) {
    throw 'Codex in-app browser environment is not ready. No acceptance scenario may be recorded.'
}
