[CmdletBinding()]
param(
    [string]$EvidenceDirectory = $env:ASTOCK_RELEASE_EVIDENCE_DIR,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$commit = (& git -C $build.RepositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Unable to resolve the fault-injection evidence source commit.'
}
if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence'
}
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null

$engine = Join-Path $build.Paths.Cargo 'release\astock-engine.exe'
$agent = Join-Path $build.Paths.MoonAgent 'native\release\build\agent_worker\agent_worker.exe'
foreach ($required in @($engine, $agent)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Fault-injection release Worker is missing: $required"
    }
}

$runId = "fault-core-$($commit.Substring(0, 12))-$([Guid]::NewGuid().ToString('N'))"
$testRoot = Join-Path $build.Paths.FormalCache $runId
New-Item -ItemType Directory -Path $testRoot -Force | Out-Null
$rootPrefix = [System.IO.Path]::GetFullPath($build.Paths.Root).TrimEnd('\') + '\'
if (-not ([System.IO.Path]::GetFullPath($testRoot) + '\').StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Fault-injection root escaped ASTOCK_BUILD_ROOT: $testRoot"
}

$started = [DateTimeOffset]::UtcNow
$cases = [System.Collections.Generic.List[object]]::new()
function Add-Case {
    param(
        [Parameter(Mandatory)][string]$Id,
        [Parameter(Mandatory)][long]$DurationMs,
        [Parameter(Mandatory)][string]$Layer
    )
    $cases.Add([pscustomobject][ordered]@{
        id = $Id
        status = 'PASSED'
        duration_ms = $DurationMs
        details = [pscustomobject][ordered]@{ layer = $Layer }
    })
}

$watch = [System.Diagnostics.Stopwatch]::StartNew()
Invoke-Checked -FilePath 'cargo' -WorkingDirectory $build.RepositoryRoot -Arguments @(
    'test', '--locked', '-p', 'astock-minimax', 'stream_break', '--', '--nocapture'
)
$watch.Stop()
Add-Case -Id 'provider-stream-break' -DurationMs $watch.ElapsedMilliseconds -Layer 'deterministic local HTTP/SSE fault server'

$watch.Restart()
Invoke-Checked -FilePath 'cargo' -WorkingDirectory $build.RepositoryRoot -Arguments @(
    'test', '--locked', '-p', 'astock-storage',
    'tests::sqlite_write_lock_fails_bounded_then_recovers_without_data_loss', '--', '--exact'
)
$watch.Stop()
Add-Case -Id 'sqlite-lock' -DurationMs $watch.ElapsedMilliseconds -Layer 'real SQLite competing BEGIN IMMEDIATE connection'

$coreOutput = & node (Join-Path $PSScriptRoot 'fault-injection-core.mjs') $engine $agent $testRoot
$coreExit = $LASTEXITCODE
if ($coreExit -ne 0) { throw 'Release Worker fault-injection process failed.' }
$core = ($coreOutput -join "`n") | ConvertFrom-Json
if (-not $core.ok) { throw 'Release Worker fault-injection process did not report success.' }
foreach ($case in $core.cases) {
    Add-Case -Id $case.id -DurationMs ([long]$case.duration_ms) -Layer 'optimized Engine/Agent framed IPC process'
}

$requiredCases = @(
    'engine-kill', 'agent-kill', 'checkpoint-before-crash', 'checkpoint-after-crash',
    'provider-stream-break', 'quota-suspension-resume', 'oversized-ipc', 'corrupt-ipc',
    'duplicate-ipc', 'out-of-order-ipc', 'cancel-safety', 'sqlite-lock'
)
$caseIds = @($cases | ForEach-Object id)
foreach ($required in $requiredCases) {
    if ($required -notin $caseIds) { throw "Core fault case was not exercised: $required" }
}
if ($caseIds.Count -ne ($caseIds | Sort-Object -Unique).Count) {
    throw 'Core fault evidence contains duplicate case identifiers.'
}

$completed = [DateTimeOffset]::UtcNow
$evidence = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'fault-injection-core'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $started.UtcDateTime.ToString('o')
    completed_at_utc = $completed.UtcDateTime.ToString('o')
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    cases = $cases
    isolation = [pscustomobject][ordered]@{
        build_root = $build.Paths.Root
        test_root = $testRoot
        production_data_touched = $false
        renderer_gpu_covered = $false
    }
}
$evidencePath = Join-Path $evidenceRoot 'fault-injection-core.json'
$json = $evidence | ConvertTo-Json -Depth 10
[System.IO.File]::WriteAllText($evidencePath, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Invoke-Checked -FilePath 'node' -WorkingDirectory $build.RepositoryRoot -Arguments @(
    (Join-Path $PSScriptRoot 'release-evidence-check.mjs'),
    $evidencePath,
    'fault-injection-core',
    $commit
)
Write-Host "Core fault-injection release evidence: $evidencePath"
