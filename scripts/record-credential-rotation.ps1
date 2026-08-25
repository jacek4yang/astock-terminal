[CmdletBinding()]
param(
    [Parameter(Mandatory)][switch]$ConfirmMinimaxRotated,
    [Parameter(Mandatory)][switch]$ConfirmJoinQuantRotated,
    [Parameter(Mandatory)][switch]$ConfirmOldCredentialsRevoked,
    [string]$EvidenceDirectory = $env:ASTOCK_RELEASE_EVIDENCE_DIR,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $ConfirmMinimaxRotated -or -not $ConfirmJoinQuantRotated -or -not $ConfirmOldCredentialsRevoked) {
    throw 'All three explicit rotation and revocation confirmations are required.'
}

. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$repository = $build.RepositoryRoot
$commit = (& git -C $repository rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Unable to resolve the credential-rotation evidence source commit.'
}
Assert-AStockCleanWorktree -RepositoryRoot $repository

if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence'
}
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null

$started = [DateTimeOffset]::UtcNow
& (Join-Path $PSScriptRoot 'build.ps1') -Component engine -Release -SkipSpaceCheck:$SkipSpaceCheck
$engine = Join-Path $build.Paths.Cargo 'release\astock-engine.exe'
if (-not (Test-Path -LiteralPath $engine -PathType Leaf)) { throw "Engine is missing: $engine" }

$watch = [System.Diagnostics.Stopwatch]::StartNew()
$readbackOutput = & node (Join-Path $PSScriptRoot 'credential-readback-smoke.mjs') $engine
if ($LASTEXITCODE -ne 0) { throw 'Credential Manager readback verification failed.' }
$watch.Stop()
$readback = ($readbackOutput -join "`n") | ConvertFrom-Json
if (-not $readback.ok -or -not $readback.minimax -or -not $readback.joinquant) {
    throw 'Credential Manager did not confirm both rotated Provider credentials.'
}

$completed = [DateTimeOffset]::UtcNow
$cases = @(
    [pscustomobject][ordered]@{
        id = 'minimax'; status = 'PASSED'; duration_ms = [long]$watch.ElapsedMilliseconds
        details = [pscustomobject][ordered]@{ operator_confirmed_rotated = $true; credential_manager_readable = $true }
    },
    [pscustomobject][ordered]@{
        id = 'joinquant'; status = 'PASSED'; duration_ms = [long]$watch.ElapsedMilliseconds
        details = [pscustomobject][ordered]@{ operator_confirmed_rotated = $true; credential_manager_readable = $true }
    }
)
$evidence = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'credential-rotation'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $started.UtcDateTime.ToString('o')
    completed_at_utc = $completed.UtcDateTime.ToString('o')
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    minimax_rotated = $true
    joinquant_rotated = $true
    old_credentials_revoked = $true
    credential_manager_readback_verified = $true
    secrets_in_evidence = $false
    attestation = 'Explicit release-operator confirmation; credential values are never read into this script.'
    cases = $cases
}
$json = $evidence | ConvertTo-Json -Depth 10
if ($json -match '(?i)sk-[a-z0-9_-]{12,}' -or $json -match '(?i)password\s*[:=]') {
    throw 'Secret-like material was detected in credential evidence.'
}
$evidencePath = Join-Path $evidenceRoot 'credential-rotation.json'
[System.IO.File]::WriteAllText($evidencePath, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Invoke-Checked -FilePath 'node' -WorkingDirectory $repository -Arguments @(
    (Join-Path $PSScriptRoot 'release-evidence-check.mjs'),
    $evidencePath,
    'credential-rotation',
    $commit
)
Write-Host "Credential rotation evidence: $evidencePath"
