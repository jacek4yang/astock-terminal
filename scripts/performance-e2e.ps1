[CmdletBinding()]
param(
    [string]$PackageDirectory,
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
    throw 'Unable to resolve the performance evidence source commit.'
}
Assert-AStockCleanWorktree -RepositoryRoot $repository

if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence'
}
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null

# Desktop automation must never run before the user's non-invasive browser
# acceptance. Revalidate the immutable evidence here, not only in the parent
# release gate, so this harness is fail-closed when invoked directly.
$browserEvidence = Join-Path $evidenceRoot 'browser-cdp.json'
Invoke-Checked -FilePath 'node' -WorkingDirectory $repository -Arguments @(
    'scripts/release-evidence-check.mjs', $browserEvidence, 'browser-cdp', $commit
)

if ([string]::IsNullOrWhiteSpace($PackageDirectory)) {
    $PackageDirectory = Join-Path $build.Paths.Artifacts 'astock-terminal'
}
$packageRoot = [System.IO.Path]::GetFullPath($PackageDirectory)
$application = Join-Path $packageRoot 'astock-terminal.exe'
$applicationZip = Join-Path $build.Paths.Artifacts 'astock-terminal.zip'
$packageMetadata = Join-Path $packageRoot 'build-metadata.json'
foreach ($required in @($application, $applicationZip, $packageMetadata)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Packaged performance prerequisite is missing: $required"
    }
}
$metadata = Get-Content -LiteralPath $packageMetadata -Raw | ConvertFrom-Json
if ($metadata.application_version -ne '6.0.0' -or $metadata.commit -ne $commit) {
    throw 'Packaged application metadata is not bound to the current v6.0.0 source commit.'
}

Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'desktop-moon') -Arguments @(
    'build', '--target', 'native', '--release', '--target-dir', $build.Paths.MoonDesktop, 'backend/skeleton'
)
$skeletonHost = Join-Path $build.Paths.MoonDesktop 'native\release\build\astock\desktop_backend\skeleton\skeleton.exe'
if (-not (Test-Path -LiteralPath $skeletonHost -PathType Leaf)) {
    throw "Pinned Proton skeleton executable is missing: $skeletonHost"
}

$runId = "performance-$($commit.Substring(0, 12))-$([Guid]::NewGuid().ToString('N'))"
$runRoot = Join-Path $build.Paths.FormalCache $runId
$skeletonArtifacts = Join-Path $runRoot 'skeleton-package'
$sampleRoot = Join-Path $runRoot 'samples'
New-Item -ItemType Directory -Path $skeletonArtifacts,$sampleRoot -Force | Out-Null
$buildPrefix = [System.IO.Path]::GetFullPath($build.Paths.Root).TrimEnd('\') + '\'
foreach ($candidate in @($runRoot, $skeletonArtifacts, $sampleRoot)) {
    $full = [System.IO.Path]::GetFullPath($candidate)
    if (-not ($full + '\').StartsWith($buildPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Performance run path escaped ASTOCK_BUILD_ROOT: $full"
    }
}

$env:ASTOCK_SKELETON_HOST_EXE = $skeletonHost
$env:ASTOCK_DESKTOP_ROOT = Join-Path $repository 'desktop-moon'
$env:ASTOCK_SKELETON_RENDERER_ROOT = Join-Path $repository 'fixtures\proton-performance-skeleton'
$env:ASTOCK_SKELETON_ARTIFACTS_DIR = $skeletonArtifacts
$env:ASTOCK_MOON_DESKTOP_TARGET = $build.Paths.MoonDesktop
Invoke-Checked -FilePath 'moon' -WorkingDirectory (Join-Path $repository 'packaging-moon') -Arguments @(
    'run', '--target', 'native', '--release', '--target-dir', $build.Paths.MoonTools, 'skeleton_packager'
)

$skeletonPackage = Join-Path $skeletonArtifacts 'astock-proton-skeleton'
$skeleton = Join-Path $skeletonPackage 'astock-proton-skeleton.exe'
$skeletonZip = Join-Path $skeletonArtifacts 'astock-proton-skeleton.zip'
foreach ($required in @($skeleton, $skeletonZip)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Packaged Proton skeleton prerequisite is missing: $required"
    }
}

$rawPath = Join-Path $sampleRoot 'performance-raw.json'
$started = [DateTimeOffset]::UtcNow
Invoke-Checked -FilePath 'node' -WorkingDirectory $repository -Arguments @(
    'scripts/performance-cdp.mjs',
    '--application', $application,
    '--skeleton', $skeleton,
    '--commit', $commit,
    '--output', $rawPath,
    '--run-root', (Join-Path $sampleRoot 'profiles')
)
$raw = Get-Content -LiteralPath $rawPath -Raw | ConvertFrom-Json
if ($raw.schema_version -ne 1 -or $raw.commit -ne $commit -or -not $raw.assertions.release_test_fixture -or -not $raw.assertions.packaged_renderer) {
    throw 'Packaged performance runner did not produce commit-bound release fixture evidence.'
}
if ([int]$raw.assertions.logical_rows -ne 100000 -or [int]$raw.assertions.maximum_dom_rows -gt 200) {
    throw 'The 100k logical-row fixture did not retain a bounded DOM.'
}

function Get-Quantile {
    param([Parameter(Mandatory)][double[]]$Values, [Parameter(Mandatory)][double]$Probability)
    if ($Values.Count -eq 0) { throw 'Cannot aggregate an empty performance sample.' }
    $sorted = @($Values | Sort-Object)
    $index = [Math]::Max(0, [Math]::Min($sorted.Count - 1, [Math]::Ceiling($Probability * $sorted.Count) - 1))
    return [double]$sorted[$index]
}

function New-Metric {
    param(
        [Parameter(Mandatory)][string]$Id,
        [Parameter(Mandatory)][double[]]$Samples,
        [Parameter(Mandatory)][string]$Comparison,
        [Parameter(Mandatory)][double]$Budget,
        [Parameter(Mandatory)][string]$Aggregation,
        [Parameter(Mandatory)][string]$Unit,
        [double[]]$BaselineSamples = @()
    )
    $value = switch ($Aggregation) {
        'p95' { Get-Quantile -Values $Samples -Probability 0.95 }
        'p05' { Get-Quantile -Values $Samples -Probability 0.05 }
        'max' { [double](($Samples | Measure-Object -Maximum).Maximum) }
        'p95_regression' {
            if ($BaselineSamples.Count -eq 0) { throw "$Id has no Proton skeleton baseline." }
            $current = Get-Quantile -Values $Samples -Probability 0.95
            $baseline = Get-Quantile -Values $BaselineSamples -Probability 0.95
            if ($baseline -le 0) { throw "$Id has an invalid Proton skeleton baseline." }
            (($current / $baseline) - 1.0) * 100.0
        }
        default { throw "Unsupported performance aggregation: $Aggregation" }
    }
    $passed = switch ($Comparison) {
        '<=' { $value -le $Budget }
        '<' { $value -lt $Budget }
        '>=' { $value -ge $Budget }
        default { throw "Unsupported performance comparison: $Comparison" }
    }
    if (-not $passed) { throw "Performance budget failed: $Id value=$value $Comparison budget=$Budget" }
    return [pscustomobject][ordered]@{
        id = $Id
        value = [Math]::Round($value, 6)
        comparison = $Comparison
        budget = $Budget
        aggregation = $Aggregation
        unit = $Unit
        status = 'PASSED'
        samples = @($Samples)
        baseline_samples = if ($Aggregation -eq 'p95_regression') { @($BaselineSamples) } else { $null }
    }
}

$metrics = @(
    New-Metric 'workspace_restore_p95_ms' @($raw.samples.workspace_restore_ms) '<=' 1500 'p95' 'ms'
    New-Metric 'command_feedback_p95_ms' @($raw.samples.command_feedback_ms) '<=' 100 'p95' 'ms'
    New-Metric 'logical_rows_scroll_fps' @($raw.samples.logical_rows_scroll_fps) '>=' 50 'p05' 'fps'
    New-Metric 'agent_render_hz' @($raw.samples.agent_render_hz) '<=' 10 'max' 'hz'
    New-Metric 'idle_cpu_p95_pct' @($raw.samples.idle_cpu_pct) '<' 2 'p95' 'pct'
    New-Metric 'cold_start_regression_pct' @($raw.samples.cold_start_ms) '<=' 15 'p95_regression' 'pct' @($raw.samples.skeleton_cold_start_ms)
    New-Metric 'memory_regression_pct' @($raw.samples.memory_bytes) '<=' 15 'p95_regression' 'pct' @($raw.samples.skeleton_memory_bytes)
)

$processor = @((Get-CimInstance Win32_Processor | ForEach-Object Name) | Sort-Object -Unique) -join '; '
$gpu = @((Get-CimInstance Win32_VideoController | ForEach-Object Name) | Sort-Object -Unique) -join '; '
$memoryBytes = [double](Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory
$powerProfile = (& powercfg.exe /getactivescheme 2>&1) -join ' '
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($powerProfile)) { throw 'Unable to resolve the active Windows power profile.' }
if (-not ('AStockReleaseDisplay' -as [type])) {
    Add-Type -TypeDefinition @'
using System.Runtime.InteropServices;
public static class AStockReleaseDisplay {
    [DllImport("user32.dll")]
    public static extern uint GetDpiForSystem();
}
'@
}
$displayScale = ([double][AStockReleaseDisplay]::GetDpiForSystem() / 96.0) * 100.0
if ([string]::IsNullOrWhiteSpace($processor) -or [string]::IsNullOrWhiteSpace($gpu) -or $memoryBytes -le 0 -or $displayScale -le 0) {
    throw 'Windows performance environment metadata is incomplete.'
}

$sourceFiles = @(
    (Join-Path $repository 'desktop-moon\backend\skeleton\main.mbt'),
    (Join-Path $repository 'desktop-moon\backend\skeleton\moon.pkg'),
    (Join-Path $repository 'fixtures\proton-performance-skeleton\index.html'),
    (Join-Path $repository 'packaging-moon\skeleton_packager\main.mbt')
)
$sourceHashInput = ($sourceFiles | ForEach-Object {
    "$(Split-Path -Leaf $_):$((Get-FileHash -Algorithm SHA256 -LiteralPath $_).Hash.ToLowerInvariant())"
}) -join "`n"
$sourceHashBytes = [System.Text.Encoding]::UTF8.GetBytes($sourceHashInput)
$sourceHasher = [System.Security.Cryptography.SHA256]::Create()
try {
    $sourceHash = ([BitConverter]::ToString($sourceHasher.ComputeHash($sourceHashBytes))).Replace('-', '').ToLowerInvariant()
} finally {
    $sourceHasher.Dispose()
}
$rawHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $rawPath).Hash.ToLowerInvariant()
$rawArtifact = [pscustomobject][ordered]@{
    kind = 'packaged-performance-raw-samples'
    path = [System.IO.Path]::GetFullPath($rawPath)
    sha256 = $rawHash
    captured_at_utc = ([DateTimeOffset]::UtcNow).UtcDateTime.ToString('o')
}
$completed = [DateTimeOffset]::UtcNow
$evidence = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'performance-budgets'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $started.UtcDateTime.ToString('o')
    completed_at_utc = $completed.UtcDateTime.ToString('o')
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant()
    }
    cases = @([pscustomobject][ordered]@{
        id = 'packaged-proton-cef-measurement'
        status = 'PASSED'
        duration_ms = [long]($completed - $started).TotalMilliseconds
        assertion_count = 7
        artifacts = @($rawArtifact)
    })
    environment = [pscustomobject][ordered]@{
        mode = 'packaged-proton-cef'
        pinned_proton_version = '0.2.1'
        cef_version = '147.0.14+g76d2442'
        chromium_version = '147.0.7727.138'
        cpu = $processor
        gpu = $gpu
        power_profile = $powerProfile.Trim()
        memory_bytes = [long]$memoryBytes
        display_scale_pct = [Math]::Round($displayScale, 3)
        proton_skeleton_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $skeletonZip).Hash.ToLowerInvariant()
        proton_skeleton_source_sha256 = $sourceHash
        application_package_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $applicationZip).Hash.ToLowerInvariant()
    }
    measurement = [pscustomobject][ordered]@{
        packaged_application = $true
        browser_preview = $false
        release_test_fixture = $true
        logical_rows = [int]$raw.assertions.logical_rows
        maximum_dom_rows = [int]$raw.assertions.maximum_dom_rows
        raw_samples = $rawArtifact
    }
    metrics = $metrics
}

$evidencePath = Join-Path $evidenceRoot 'performance.json'
$candidatePath = Join-Path $evidenceRoot ".performance-$($commit.Substring(0, 12))-$([Guid]::NewGuid().ToString('N')).candidate.json"
[System.IO.File]::WriteAllText(
    $candidatePath,
    ($evidence | ConvertTo-Json -Depth 12) + [Environment]::NewLine,
    [System.Text.UTF8Encoding]::new($false)
)
try {
    Invoke-Checked -FilePath 'node' -WorkingDirectory $repository -Arguments @(
        'scripts/release-evidence-check.mjs', $candidatePath, 'performance-budgets', $commit
    )
    if (Test-Path -LiteralPath $evidencePath -PathType Leaf) {
        $backupPath = Join-Path $evidenceRoot "performance.previous-$([DateTimeOffset]::UtcNow.ToString('yyyyMMddTHHmmssZ')).json"
        [System.IO.File]::Replace($candidatePath, $evidencePath, $backupPath)
    } else {
        [System.IO.File]::Move($candidatePath, $evidencePath)
    }
} finally {
    if (Test-Path -LiteralPath $candidatePath -PathType Leaf) {
        Remove-Item -LiteralPath $candidatePath -Force
    }
}
Get-FileHash -Algorithm SHA256 -LiteralPath $evidencePath | Format-List
