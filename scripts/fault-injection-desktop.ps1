[CmdletBinding()]
param(
    [string]$PackageDirectory,
    [string]$EvidenceDirectory,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$commit = (& git -C $build.RepositoryRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') { throw 'Unable to resolve the desktop fault-injection source commit.' }
if ([string]::IsNullOrWhiteSpace($PackageDirectory)) { $PackageDirectory = Join-Path $build.Paths.Artifacts 'astock-terminal' }
if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) { $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence' }
$packageRoot = [System.IO.Path]::GetFullPath($PackageDirectory)
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
$buildPrefix = [System.IO.Path]::GetFullPath($build.Paths.Root).TrimEnd('\') + '\'
foreach ($path in @($packageRoot, $evidenceRoot)) {
    if (-not (($path.TrimEnd('\') + '\').StartsWith($buildPrefix, [System.StringComparison]::OrdinalIgnoreCase))) {
        throw "Desktop fault input/output must remain under ASTOCK_BUILD_ROOT: $path"
    }
}
New-Item -ItemType Directory -Path $evidenceRoot -Force | Out-Null

$corePath = Join-Path $evidenceRoot 'fault-injection-core.json'
if (-not (Test-Path -LiteralPath $corePath -PathType Leaf)) { throw 'Core fault evidence must pass before desktop fault injection.' }
$core = Get-Content -LiteralPath $corePath -Raw | ConvertFrom-Json
if ($core.commit -ne $commit -or $core.status -ne 'PASSED') { throw 'Core fault evidence is stale or failed.' }
$started = [DateTimeOffset]::UtcNow

function Write-ResultArtifact {
    param([Parameter(Mandatory)][string]$Name, [Parameter(Mandatory)][object]$Value)
    $path = Join-Path $evidenceRoot $Name
    $json = $Value | ConvertTo-Json -Depth 12
    [System.IO.File]::WriteAllText($path, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
    return [pscustomobject][ordered]@{
        kind = 'cdp-fault-trace'
        path = $path
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
        captured_at_utc = [DateTimeOffset]::UtcNow.UtcDateTime.ToString('o')
    }
}

$rendererStarted = [DateTimeOffset]::UtcNow
$rendererOutput = & (Join-Path $PSScriptRoot 'desktop-cdp-session.ps1') `
    -PackageDirectory $packageRoot -Mode renderer-fault -Headless -SkipSpaceCheck
if ($LASTEXITCODE -ne 0) { throw 'Packaged renderer crash recovery failed.' }
$renderer = ($rendererOutput -join "`n") | ConvertFrom-Json
if ($renderer.ok -ne $true -or $renderer.renderer_fault_injected -ne $true -or $renderer.host_restart_required -ne $false) {
    throw 'Renderer fault trace does not prove in-place recovery.'
}
$rendererArtifact = Write-ResultArtifact -Name 'renderer-fault.json' -Value $renderer
$rendererCompleted = [DateTimeOffset]::UtcNow

$gpuStarted = [DateTimeOffset]::UtcNow
$gpuOutput = & (Join-Path $PSScriptRoot 'desktop-cdp-session.ps1') `
    -PackageDirectory $packageRoot -Mode smoke -Headless -SkipSpaceCheck
if ($LASTEXITCODE -ne 0) { throw 'Packaged software GPU fallback failed.' }
$gpu = ($gpuOutput -join "`n") | ConvertFrom-Json
if ($gpu.ok -ne $true -or $gpu.canvas_2d_available -ne $true) { throw 'Software GPU fallback did not retain CEF canvas rendering.' }
$gpuTrace = [pscustomobject][ordered]@{
    result = $gpu
    injected_condition = 'PROTON_DISABLE_GPU=1'
    expected_behavior = 'CEF React and Canvas2D remain usable through software compositing'
}
$gpuArtifact = Write-ResultArtifact -Name 'gpu-fallback.json' -Value $gpuTrace
$gpuCompleted = [DateTimeOffset]::UtcNow

$cases = @($core.cases) + @(
    [pscustomobject][ordered]@{
        id = 'renderer-kill'
        status = 'PASSED'
        duration_ms = [Math]::Round(($rendererCompleted - $rendererStarted).TotalMilliseconds, 3)
        assertion_count = 5
        artifacts = @($rendererArtifact)
    },
    [pscustomobject][ordered]@{
        id = 'gpu-failure'
        status = 'PASSED'
        duration_ms = [Math]::Round(($gpuCompleted - $gpuStarted).TotalMilliseconds, 3)
        assertion_count = 4
        artifacts = @($gpuArtifact)
    }
)
$completed = [DateTimeOffset]::UtcNow
$evidence = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'fault-injection'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $started.UtcDateTime.ToString('o')
    completed_at_utc = $completed.UtcDateTime.ToString('o')
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    cases = $cases
    package_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $build.Paths.Artifacts 'astock-terminal-setup.exe')).Hash.ToLowerInvariant()
    core_evidence_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $corePath).Hash.ToLowerInvariant()
    isolation = [pscustomobject][ordered]@{
        build_root = $build.Paths.Root
        production_data_touched = $false
        renderer_gpu_covered = $true
    }
}
$evidencePath = Join-Path $evidenceRoot 'fault-injection.json'
[System.IO.File]::WriteAllText($evidencePath, ($evidence | ConvertTo-Json -Depth 12) + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Invoke-Checked -FilePath 'node' -WorkingDirectory $build.RepositoryRoot -Arguments @(
    (Join-Path $PSScriptRoot 'release-evidence-check.mjs'), $evidencePath, 'fault-injection', $commit
)
Write-Host "Desktop fault-injection release evidence: $evidencePath"
