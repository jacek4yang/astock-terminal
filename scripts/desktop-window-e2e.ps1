[CmdletBinding()]
param(
    [string]$PackageDirectory,
    [string]$EvidenceDirectory,
    [switch]$AllowInteractiveInput,
    [switch]$SkipSpaceCheck
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Build.Common.ps1')
$build = Initialize-AStockBuildEnvironment -SkipSpaceCheck:$SkipSpaceCheck
$repository = $build.RepositoryRoot
$commit = (& git -C $repository rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $commit -notmatch '^[a-f0-9]{40}$') {
    throw 'Unable to resolve the native window test source commit.'
}
Assert-AStockCleanWorktree -RepositoryRoot $repository
if (-not $AllowInteractiveInput) {
    throw 'Native window acceptance sends bounded mouse input to the isolated test window and requires -AllowInteractiveInput.'
}
if ([string]::IsNullOrWhiteSpace($PackageDirectory)) {
    $PackageDirectory = Join-Path $build.Paths.Artifacts 'astock-terminal'
}
if ([string]::IsNullOrWhiteSpace($EvidenceDirectory)) {
    $EvidenceDirectory = Join-Path $build.Paths.Artifacts 'release-evidence'
}
$packageRoot = [System.IO.Path]::GetFullPath($PackageDirectory)
$evidenceRoot = [System.IO.Path]::GetFullPath($EvidenceDirectory)
$buildPrefix = [System.IO.Path]::GetFullPath($build.Paths.Root).TrimEnd('\') + '\'
foreach ($path in @($packageRoot, $evidenceRoot)) {
    if (-not (($path.TrimEnd('\') + '\').StartsWith($buildPrefix, [System.StringComparison]::OrdinalIgnoreCase))) {
        throw "Native window inputs and evidence must remain under ASTOCK_BUILD_ROOT: $path"
    }
}

$host = Join-Path $packageRoot 'astock-terminal.exe'
$metadataPath = Join-Path $packageRoot 'Resources\build-metadata.json'
foreach ($required in @($host, $metadataPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Native window test input is missing: $required"
    }
}
$metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
if ($metadata.commit -ne $commit -or $metadata.application_version -ne '6.0.0') {
    throw 'Native window test package is not bound to the current v6.0.0 commit.'
}

$runRoot = Join-Path $build.Paths.FormalCache "desktop-window-$($commit.Substring(0, 12))-$([guid]::NewGuid().ToString('N'))"
$dataRoot = Join-Path $runRoot 'data'
$localAppData = Join-Path $runRoot 'local'
$appData = Join-Path $runRoot 'roaming'
$rawRoot = Join-Path $evidenceRoot 'desktop-window'
New-Item -ItemType Directory -Path $dataRoot,$localAppData,$appData,$rawRoot -Force | Out-Null

function Assert-WindowCondition {
    param([Parameter(Mandatory)][bool]$Condition, [Parameter(Mandatory)][string]$Message)
    if (-not $Condition) { throw $Message }
}

function Invoke-WindowProbe {
    param(
        [Parameter(Mandatory)][string]$Operation,
        [hashtable]$Arguments = @{}
    )
    $parameters = @{
        ProcessId = $process.Id
        ExpectedExecutablePath = $host
        Operation = $Operation
        TimeoutMs = 15000
        SkipSpaceCheck = $true
    }
    foreach ($entry in $Arguments.GetEnumerator()) { $parameters[$entry.Key] = $entry.Value }
    $output = & (Join-Path $PSScriptRoot 'desktop-window-probe.ps1') @parameters
    if ($LASTEXITCODE -ne 0) { throw "Native window probe failed for $Operation." }
    return ($output -join "`n") | ConvertFrom-Json
}

function Write-WindowCase {
    param(
        [Parameter(Mandatory)][string]$Id,
        [Parameter(Mandatory)][datetimeoffset]$Started,
        [Parameter(Mandatory)][int]$AssertionCount,
        [Parameter(Mandatory)][object]$Trace
    )
    $path = Join-Path $rawRoot "$Id.json"
    $payload = [pscustomobject][ordered]@{
        schema_version = 1
        commit = $commit
        case_id = $Id
        status = 'PASSED'
        started_at_utc = $Started.UtcDateTime.ToString('o')
        completed_at_utc = [DateTimeOffset]::UtcNow.UtcDateTime.ToString('o')
        trace = $Trace
    }
    [System.IO.File]::WriteAllText($path, ($payload | ConvertTo-Json -Depth 12) + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
    return [pscustomobject][ordered]@{
        id = $Id
        status = 'PASSED'
        duration_ms = [Math]::Round(([DateTimeOffset]::UtcNow - $Started).TotalMilliseconds, 3)
        assertion_count = $AssertionCount
        artifacts = @([pscustomobject][ordered]@{
            kind = 'win32-window-trace'
            path = $path
            sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLowerInvariant()
            captured_at_utc = [DateTimeOffset]::UtcNow.UtcDateTime.ToString('o')
        })
    }
}

$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $host
$startInfo.WorkingDirectory = $packageRoot
$startInfo.UseShellExecute = $false
$startInfo.CreateNoWindow = $false
$startInfo.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Normal
$startInfo.RedirectStandardError = $true
$startInfo.Environment['ASTOCK_DATA_DIR'] = $dataRoot
$startInfo.Environment['LOCALAPPDATA'] = $localAppData
$startInfo.Environment['APPDATA'] = $appData
$process = [System.Diagnostics.Process]::new()
$process.StartInfo = $startInfo
if (-not $process.Start()) { throw 'Unable to launch the packaged Proton/CEF application for native window acceptance.' }
$stderrTask = $process.StandardError.ReadToEndAsync()

$cases = [System.Collections.Generic.List[object]]::new()
$suiteStarted = [DateTimeOffset]::UtcNow
try {
    $started = [DateTimeOffset]::UtcNow
    $initial = Invoke-WindowProbe -Operation inspect
    Assert-WindowCondition ($initial.Visible -eq $true) 'Packaged Proton window is not visible.'
    Assert-WindowCondition ($initial.ClassName -match 'Proton') 'Packaged top-level window is not a Proton window.'
    Assert-WindowCondition ($initial.Title -eq 'AStock Terminal') 'Packaged top-level window title is incorrect.'
    $cases.Add((Write-WindowCase -Id 'packaged-launch' -Started $started -AssertionCount 3 -Trace @{ initial = $initial }))

    $started = [DateTimeOffset]::UtcNow
    Assert-WindowCondition ($initial.Resizable -eq $true) 'Packaged window is missing WS_THICKFRAME.'
    Assert-WindowCondition ($initial.HasMinimizeBox -eq $true) 'Packaged window is missing its minimize capability.'
    Assert-WindowCondition ($initial.HasMaximizeBox -eq $true) 'Packaged window is missing its maximize capability.'
    Assert-WindowCondition ($initial.TaskbarEligible -eq $true) 'Packaged window is not eligible for the Windows taskbar.'
    Assert-WindowCondition ($initial.HasLargeIcon -eq $true -and $initial.HasSmallIcon -eq $true) 'Packaged window is missing a large or small native icon.'
    Assert-WindowCondition ([int]$initial.Dpi -ge 96) 'GetDpiForWindow returned an invalid DPI.'
    $cases.Add((Write-WindowCase -Id 'taskbar-icon-high-dpi' -Started $started -AssertionCount 6 -Trace @{ initial = $initial }))

    $baseline = Invoke-WindowProbe -Operation move-resize -Arguments @{ X = 120; Y = 100; Width = 1100; Height = 680 }
    Assert-WindowCondition (-not $baseline.Maximized -and -not $baseline.Minimized) 'Window did not enter the restored baseline state.'

    $scale = [Math]::Max(1.0, [double]$baseline.Dpi / 96.0)
    $brandX = [int]([double]$baseline.ClientOriginX + (80.0 * $scale))
    $brandY = [int]([double]$baseline.ClientOriginY + (21.0 * $scale))

    $started = [DateTimeOffset]::UtcNow
    $dragged = Invoke-WindowProbe -Operation interactive-drag -Arguments @{
        StartX = $brandX; StartY = $brandY
        EndX = $brandX + [int](110 * $scale); EndY = $brandY + [int](65 * $scale)
        AllowInteractiveInput = $true
    }
    Assert-WindowCondition ([Math]::Abs([int]$dragged.X - [int]$baseline.X) -ge 40) 'Interactive titlebar drag did not move the window horizontally.'
    Assert-WindowCondition ([Math]::Abs([int]$dragged.Y - [int]$baseline.Y) -ge 20) 'Interactive titlebar drag did not move the window vertically.'
    Assert-WindowCondition ([Math]::Abs([int]$dragged.Width - [int]$baseline.Width) -le 4 -and [Math]::Abs([int]$dragged.Height - [int]$baseline.Height) -le 4) 'Interactive titlebar drag unexpectedly resized the window.'
    $cases.Add((Write-WindowCase -Id 'window-drag' -Started $started -AssertionCount 3 -Trace @{ before = $baseline; after = $dragged }))

    $brandX = [int]([double]$dragged.ClientOriginX + (80.0 * $scale))
    $brandY = [int]([double]$dragged.ClientOriginY + (21.0 * $scale))
    $started = [DateTimeOffset]::UtcNow
    $maximized = Invoke-WindowProbe -Operation interactive-double-click -Arguments @{ StartX = $brandX; StartY = $brandY; AllowInteractiveInput = $true }
    Assert-WindowCondition ($maximized.Maximized -eq $true -and $maximized.Minimized -eq $false) 'Double-clicking the custom titlebar did not maximize the window.'
    $cases.Add((Write-WindowCase -Id 'window-double-click-maximize' -Started $started -AssertionCount 2 -Trace @{ before = $dragged; after = $maximized }))

    $started = [DateTimeOffset]::UtcNow
    $maxBrandX = [int]([double]$maximized.ClientOriginX + (80.0 * $scale))
    $maxBrandY = [int]([double]$maximized.ClientOriginY + (21.0 * $scale))
    $restored = Invoke-WindowProbe -Operation interactive-double-click -Arguments @{ StartX = $maxBrandX; StartY = $maxBrandY; AllowInteractiveInput = $true }
    Assert-WindowCondition ($restored.Maximized -eq $false -and $restored.Minimized -eq $false) 'A second custom-titlebar double-click did not restore the window.'
    Assert-WindowCondition ([int]$restored.Width -ge 1000 -and [int]$restored.Height -ge 600) 'Restored window lost its usable dimensions.'
    $cases.Add((Write-WindowCase -Id 'window-restore' -Started $started -AssertionCount 3 -Trace @{ before = $maximized; after = $restored }))

    $started = [DateTimeOffset]::UtcNow
    $resized = Invoke-WindowProbe -Operation interactive-edge-resize -Arguments @{
        EndX = [int]$restored.X + [int]$restored.Width + 80
        EndY = [int]$restored.Y + [int]$restored.Height + 40
        AllowInteractiveInput = $true
    }
    Assert-WindowCondition ([int]$resized.Width -ge [int]$restored.Width + 20) 'Interactive right-edge resize did not increase the window width.'
    Assert-WindowCondition ([int]$resized.Height -ge [int]$restored.Height + 10) 'Interactive bottom-edge resize did not increase the window height.'
    Assert-WindowCondition ($resized.Resizable -eq $true) 'Window lost its resizable style after edge resize.'
    $cases.Add((Write-WindowCase -Id 'window-edge-resize' -Started $started -AssertionCount 3 -Trace @{ before = $restored; after = $resized }))

    $started = [DateTimeOffset]::UtcNow
    $minimized = Invoke-WindowProbe -Operation minimize
    Assert-WindowCondition ($minimized.Minimized -eq $true -and $minimized.Maximized -eq $false) 'Native minimize did not iconify the packaged window.'
    $cases.Add((Write-WindowCase -Id 'window-minimize' -Started $started -AssertionCount 2 -Trace @{ before = $resized; after = $minimized }))
    [void](Invoke-WindowProbe -Operation restore)

    $stable = Invoke-WindowProbe -Operation inspect
    $brandX = [int]([double]$stable.ClientOriginX + (80.0 * $scale))
    $brandY = [int]([double]$stable.ClientOriginY + (21.0 * $scale))
    $started = [DateTimeOffset]::UtcNow
    $menuClosed = Invoke-WindowProbe -Operation interactive-context-menu -Arguments @{ StartX = $brandX; StartY = $brandY; AllowInteractiveInput = $true }
    Assert-WindowCondition ($menuClosed.Visible -eq $true -and -not $menuClosed.Minimized) 'Titlebar system-menu interaction left the window unusable.'
    Assert-WindowCondition (-not $process.HasExited) 'Titlebar system-menu interaction terminated the packaged application.'
    $cases.Add((Write-WindowCase -Id 'native-context-menu' -Started $started -AssertionCount 3 -Trace @{
        before = $stable
        after = $menuClosed
        process_alive = (-not $process.HasExited)
    }))
} finally {
    try {
        if (-not $process.HasExited) {
            [void](Invoke-WindowProbe -Operation restore)
            [void]$process.CloseMainWindow()
            if (-not $process.WaitForExit(5000)) { $process.Kill($true) }
        }
    } catch {
        if (-not $process.HasExited) { $process.Kill($true) }
    }
}

$stderrText = $stderrTask.GetAwaiter().GetResult()
$stderrLines = @($stderrText -split "`r?`n" | Where-Object { $_ } | Select-Object -First 512)
$summary = [pscustomobject][ordered]@{
    schema_version = 1
    gate = 'desktop-window-native'
    status = 'PASSED'
    commit = $commit
    started_at_utc = $suiteStarted.UtcDateTime.ToString('o')
    completed_at_utc = [DateTimeOffset]::UtcNow.UtcDateTime.ToString('o')
    package_executable = $host
    package_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $host).Hash.ToLowerInvariant()
    runner = [pscustomobject][ordered]@{
        os = [System.Environment]::OSVersion.VersionString
        arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
    }
    cases = $cases
    isolation = [pscustomobject][ordered]@{
        build_root = $build.Paths.Root
        data_root = $dataRoot
        production_data_touched = $false
        interactive_input_bounded = $true
        cursor_position_restored = $true
    }
    stderr = $stderrLines
}
$summaryPath = Join-Path $evidenceRoot 'desktop-window-native.json'
[System.IO.File]::WriteAllText($summaryPath, ($summary | ConvertTo-Json -Depth 12) + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
Write-Host "Native desktop window evidence: $summaryPath"
