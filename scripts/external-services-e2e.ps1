[CmdletBinding()]
param(
    [string]$EvidenceDirectory = $env:ASTOCK_RELEASE_EVIDENCE_DIR,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$repository = $build.RepositoryRoot
$commit = (& git -C $repository rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Unable to resolve the external-services evidence source commit.'
}
Assert-AStockCleanWorktree -RepositoryRoot $repository

if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence'
}
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null

# Never touch Provider credentials until the user has rotated both exposed
# credentials, revoked the old values and verified Credential Manager readback.
$rotationEvidence = Join-Path $evidenceRoot 'credential-rotation.json'
if (-not (Test-Path -LiteralPath $rotationEvidence -PathType Leaf)) {
    throw 'Credential rotation evidence is required before any live Provider request.'
}
Invoke-Checked -FilePath 'node' -WorkingDirectory $repository -Arguments @(
    (Join-Path $PSScriptRoot 'release-evidence-check.mjs'),
    $rotationEvidence,
    'credential-rotation',
    $commit
)

$started = [DateTimeOffset]::UtcNow
& (Join-Path $PSScriptRoot 'build.ps1') -Component engine -Release -SkipSpaceCheck:$SkipSpaceCheck
& (Join-Path $PSScriptRoot 'build.ps1') -Component agent -Release -SkipSpaceCheck:$SkipSpaceCheck
Set-AStockWorkerEnvironment -Environment $build -Release

$streamWatch = [System.Diagnostics.Stopwatch]::StartNew()
Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'app-moon') -Arguments @(
    'test', '--target', 'native', '--target-dir', $build.Paths.MoonAgent,
    '-p', 'astock/terminal/agent_worker', '-f', '*stream*'
)
$streamWatch.Stop()

$liveOutput = & node (Join-Path $PSScriptRoot 'research-live-smoke.mjs') $env:ASTOCK_ENGINE_EXE $env:ASTOCK_AGENT_EXE
if ($LASTEXITCODE -ne 0) {
    throw 'MiniMax Plus and JoinQuant live acceptance failed.'
}
$live = ($liveOutput -join "`n") | ConvertFrom-Json
if (-not $live.ok) { throw 'Live Provider runner did not report success.' }
if ($live.stream.transport -ne 'sse' -or -not $live.stream.real_stream_completed) {
    throw 'The real MiniMax research report did not complete over SSE.'
}
if ($live.manual_plan.phase -ne 'completed' -or $live.manual_plan.verifier_version -ne 'engine-report-verifier-v1') {
    throw 'The 20,000 CNY manual plan did not pass the independent Engine verifier.'
}
if ($live.joinquant.configured -ne $true -or $live.joinquant.row_count -lt 1) {
    throw 'JoinQuant did not return a usable authenticated dataset.'
}

$cases = @(
    [pscustomobject][ordered]@{
        id = 'minimax-provider-discovery'; status = 'PASSED'; duration_ms = [long]$live.provider.duration_ms
        details = [pscustomobject][ordered]@{
            catalog_verified = [bool]$live.provider.catalog_verified
            model = [string]$live.provider.model
            model_count = [int]$live.provider.model_count
            api_region = [string]$live.provider.api_region
        }
    },
    [pscustomobject][ordered]@{
        id = 'minimax-20000-manual-plan'; status = 'PASSED'; duration_ms = [long]$live.manual_plan.duration_ms
        details = [pscustomobject][ordered]@{
            capital_cny = [int]$live.manual_plan.capital_cny
            phase = [string]$live.manual_plan.phase
            model_rounds = [int]$live.manual_plan.model_rounds
            evidence_count = [int]$live.manual_plan.evidence_count
            report_chars = [int]$live.manual_plan.report_chars
            report_sha256 = [string]$live.manual_plan.report_sha256
            verifier_version = [string]$live.manual_plan.verifier_version
            numeric_claims_checked = [int]$live.manual_plan.numeric_claims_checked
            distinct_citations = [int]$live.manual_plan.distinct_citations
        }
    },
    [pscustomobject][ordered]@{
        id = 'minimax-stream-resume'; status = 'PASSED'; duration_ms = [long]($streamWatch.ElapsedMilliseconds + $live.manual_plan.duration_ms)
        details = [pscustomobject][ordered]@{
            implementation = 'moonbit-agent-worker'
            transport = 'sse'
            real_stream_completed = $true
            incomplete_stream_rejected = $true
            partial_output_discarded = $true
            complete_response_retry_tested = $true
        }
    },
    [pscustomobject][ordered]@{
        id = 'minimax-quota'; status = 'PASSED'; duration_ms = [long]$live.quota.duration_ms
        details = [pscustomobject][ordered]@{
            model_count = [int]$live.quota.model_count
            fetched_at_ms = [long]$live.quota.fetched_at_ms
        }
    },
    [pscustomobject][ordered]@{
        id = 'joinquant-auth'; status = 'PASSED'; duration_ms = [long]$live.joinquant.credential_status_duration_ms
        details = [pscustomobject][ordered]@{ configured = [bool]$live.joinquant.configured }
    },
    [pscustomobject][ordered]@{
        id = 'joinquant-minimal-data'; status = 'PASSED'; duration_ms = [long]$live.joinquant.duration_ms
        details = [pscustomobject][ordered]@{
            dataset = [string]$live.joinquant.dataset
            row_count = [int]$live.joinquant.row_count
            total_rows = [int]$live.joinquant.total_rows
            source = [string]$live.joinquant.source
            fetched_at = [string]$live.joinquant.fetched_at
            symbol = [string]$live.joinquant.symbol
            requested_start = [string]$live.joinquant.requested_start
            requested_end = [string]$live.joinquant.requested_end
            first_date = [string]$live.joinquant.first_date
            latest_date = [string]$live.joinquant.latest_date
            latest_lag_days = [int]$live.joinquant.latest_lag_days
            structural_rows_checked = [int]$live.joinquant.structural_rows_checked
            volume_unit = [string]$live.joinquant.volume_unit
            truncated = [bool]$live.joinquant.truncated
            data_sha256 = [string]$live.joinquant.data_sha256
        }
    }
)

$completed = [DateTimeOffset]::UtcNow
$evidence = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'minimax-plus-joinquant-live'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $started.UtcDateTime.ToString('o')
    completed_at_utc = $completed.UtcDateTime.ToString('o')
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    trusted_boundary = $true
    secrets_in_evidence = $false
    cases = $cases
}

$json = $evidence | ConvertTo-Json -Depth 12
if ($json -match '(?i)sk-[a-z0-9_-]{12,}' -or $json -match '(?i)password\s*[:=]') {
    throw 'Secret-like material was detected in external service evidence.'
}
$evidencePath = Join-Path $evidenceRoot 'external-services.json'
[System.IO.File]::WriteAllText($evidencePath, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Invoke-Checked -FilePath 'node' -WorkingDirectory $repository -Arguments @(
    (Join-Path $PSScriptRoot 'release-evidence-check.mjs'),
    $evidencePath,
    'minimax-plus-joinquant-live',
    $commit
)
Write-Host "External Provider evidence: $evidencePath"
